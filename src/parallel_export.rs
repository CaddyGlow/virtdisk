//! Bounded immutable preparation with one serial private-output coordinator.
use crate::{OperationContext, OperationEvent, OperationPhase, ReadAt, WriteAt, io};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc,
};

type Job = (u64, usize, Vec<u8>);
type Prepared = io::Result<(u64, usize, Vec<u8>, bool)>;

fn charge_reads(context: &mut OperationContext<'_>, attempts: &AtomicU64, charged: &mut u64) {
    let current = attempts.load(Ordering::Acquire);
    for _ in *charged..current {
        context.attempted_io();
    }
    *charged = current;
}

pub(crate) fn export_raw(
    source: &dyn ReadAt,
    writer: &dyn WriteAt,
    sparse: bool,
    context: &mut OperationContext<'_>,
) -> io::Result<()> {
    if writer.len() != source.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private export capacity mismatch",
        ));
    }
    let workers = context.workers();
    let size = source.len();
    let chunk = context.preflight(size, workers, 2)?;
    context.observe_phase(OperationPhase::ImageExport, 0, size)?;
    if size == 0 {
        return Ok(());
    }
    context.scratch(chunk * workers);
    let mut buffers = Vec::new();
    for _ in 0..workers {
        buffers.push(crate::operation_context::scratch_buffer(chunk)?);
    }
    let attempts = AtomicU64::new(0);
    let mut charged = 0;
    let profiler = context.profiler().cloned();
    std::thread::scope(|scope| {
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        let mut handles = Vec::new();
        let mut outcome = Ok(());
        for _ in 0..workers {
            let (sender, jobs) = mpsc::sync_channel::<Job>(1);
            let (results, receiver) = mpsc::sync_channel::<Prepared>(1);
            let attempts = &attempts;
            let profiler = profiler.clone();
            match std::thread::Builder::new()
                .name("virtdisk-prepare".into())
                .spawn_scoped(scope, move || {
                    while let Ok((offset, count, mut buffer)) = jobs.recv() {
                        attempts.fetch_add(1, Ordering::Release);
                        let mut read = || source.read_exact_at(offset, &mut buffer[..count]);
                        let result = if let Some(profiler) = &profiler {
                            profiler.measure(OperationEvent::SourceRead, count as u64, read)
                        } else {
                            read()
                        };
                        let prepared = result.map(|()| {
                            let started = profiler.as_ref().map(|_| std::time::Instant::now());
                            let nonzero = !sparse || buffer[..count].iter().any(|byte| *byte != 0);
                            if sparse && let (Some(profiler), Some(started)) = (&profiler, started)
                            {
                                profiler.record(
                                    OperationEvent::ZeroDetection,
                                    count as u64,
                                    started,
                                    true,
                                );
                            }
                            (offset, count, buffer, nonzero)
                        });
                        let failed = prepared.is_err();
                        if results.send(prepared).is_err() || failed {
                            break;
                        }
                    }
                }) {
                Ok(handle) => {
                    senders.push(sender);
                    receivers.push(receiver);
                    handles.push(handle);
                }
                Err(error) => {
                    outcome = Err(error.into());
                    break;
                }
            }
        }
        if outcome.is_ok() {
            outcome = (|| {
                let mut next = 0;
                for (sender, buffer) in senders.iter().zip(buffers) {
                    if next >= size {
                        break;
                    }
                    let count = (size - next).min(chunk as u64) as usize;
                    sender
                        .send((next, count, buffer))
                        .map_err(|_| io::Error::other("preparation worker stopped"))?;
                    next += count as u64;
                }
                let mut offset = 0;
                let mut lane = 0;
                while offset < size {
                    let (prepared_offset, count, buffer, nonzero) =
                        receivers[lane].recv().map_err(|_| {
                            io::Error::other("preparation worker panicked or stopped")
                        })??;
                    charge_reads(context, &attempts, &mut charged);
                    if prepared_offset != offset {
                        return Err(io::Error::other("preparation order mismatch"));
                    }
                    if nonzero {
                        context.attempted_io();
                        if let Some(profiler) = &profiler {
                            profiler.measure(OperationEvent::PayloadWrite, count as u64, || {
                                writer.write_all_at(offset, &buffer[..count])
                            })?;
                        } else {
                            writer.write_all_at(offset, &buffer[..count])?;
                        }
                    }
                    offset += count as u64;
                    context.completed(count as u64);
                    context.observe_phase(OperationPhase::ImageExport, offset, size)?;
                    if next < size {
                        let count = (size - next).min(chunk as u64) as usize;
                        senders[lane]
                            .send((next, count, buffer))
                            .map_err(|_| io::Error::other("preparation worker stopped"))?;
                        next += count as u64;
                    }
                    lane = (lane + 1) % workers;
                }
                Ok(())
            })();
        }
        // Closing both directions releases idle senders/receivers before joining.
        // An in-flight immutable read must finish; the original error wins.
        drop(senders);
        drop(receivers);
        for handle in handles {
            if handle.join().is_err() && outcome.is_ok() {
                outcome = Err(io::Error::other("preparation worker panicked"));
            }
        }
        charge_reads(context, &attempts, &mut charged);
        outcome
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordinator_write_failure_joins_readers_and_preserves_original_error() {
        struct Source {
            reads: AtomicU64,
            active: AtomicU64,
        }
        impl ReadAt for Source {
            fn len(&self) -> u64 {
                1048576
            }
            fn read_exact_at(&self, _: u64, bytes: &mut [u8]) -> io::Result<()> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.active.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                bytes.fill(41);
                self.active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }
        }
        struct Writer;
        impl WriteAt for Writer {
            fn len(&self) -> u64 {
                1048576
            }
            fn write_all_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
                Err(std::io::Error::from_raw_os_error(if cfg!(windows) { 112 } else { 28 }).into())
            }
            fn flush(&self) -> io::Result<()> {
                panic!("failure must not flush")
            }
        }
        let source = Source {
            reads: AtomicU64::new(0),
            active: AtomicU64::new(0),
        };
        let profiler = crate::OperationProfiler::default();
        let mut context = OperationContext::default()
            .with_workers(4)
            .unwrap()
            .with_profiler(profiler.clone());
        let error = export_raw(&source, &Writer, true, &mut context).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(if cfg!(windows) { 112 } else { 28 })
        );
        assert_eq!(source.active.load(Ordering::SeqCst), 0);
        assert!(source.reads.load(Ordering::SeqCst) <= 4);
        assert_eq!(context.usage().logical_bytes, 0);
        assert_eq!(
            context.usage().io_operations,
            source.reads.load(Ordering::SeqCst) + 1
        );
        assert_eq!(profiler.metrics(OperationEvent::PayloadWrite).failures, 1);
    }
}
