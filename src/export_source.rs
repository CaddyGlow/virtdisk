//! Synchronous source access inside native exporters.
use crate::io;
use crate::{OperationContext, OperationPhase, ReadAt};

pub(crate) trait ExportSource {
    fn streaming_allocation(&self) -> bool {
        false
    }
    fn profiler(&self) -> Option<crate::OperationProfiler> {
        None
    }
    fn nonzero(&self, bytes: &[u8]) -> bool {
        let profiler = self.profiler();
        let started = profiler.as_ref().map(|_| std::time::Instant::now());
        let nonzero = bytes.iter().any(|byte| *byte != 0);
        if let (Some(profiler), Some(started)) = (profiler, started) {
            profiler.record(
                crate::OperationEvent::ZeroDetection,
                bytes.len() as u64,
                started,
                true,
            );
        }
        nonzero
    }
    fn size(&self) -> u64;
    fn begin(&mut self, phase: OperationPhase, bytes: u64) -> io::Result<()>;
    fn read(&mut self, offset: u64, destination: &mut [u8]) -> io::Result<()>;
}
impl ExportSource for &dyn ReadAt {
    fn size(&self) -> u64 {
        self.len()
    }
    fn begin(&mut self, _: OperationPhase, _: u64) -> io::Result<()> {
        Ok(())
    }
    fn read(&mut self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.read_exact_at(offset, destination)
    }
}
pub(crate) struct ContextSource<'a, 'observer> {
    source: &'a dyn ReadAt,
    context: &'a mut OperationContext<'observer>,
    phase: OperationPhase,
    completed: u64,
    total: u64,
    read_chunk_bytes: u64,
}
impl<'a, 'observer> ContextSource<'a, 'observer> {
    pub(crate) fn new(
        source: &'a dyn ReadAt,
        context: &'a mut OperationContext<'observer>,
        read_chunk_bytes: u64,
    ) -> Self {
        Self {
            source,
            context,
            phase: OperationPhase::ImageExport,
            completed: 0,
            total: 0,
            read_chunk_bytes,
        }
    }
}
impl ExportSource for ContextSource<'_, '_> {
    fn streaming_allocation(&self) -> bool {
        self.context.streaming_allocation(self.source.len())
    }
    fn profiler(&self) -> Option<crate::OperationProfiler> {
        self.context.profiler().cloned()
    }
    fn size(&self) -> u64 {
        self.source.len()
    }
    fn begin(&mut self, phase: OperationPhase, bytes: u64) -> io::Result<()> {
        self.context
            .preflight_chunks(bytes, self.read_chunk_bytes, 1)?;
        self.phase = phase;
        self.completed = 0;
        self.total = bytes;
        self.context.observe_phase(phase, 0, bytes)
    }
    fn read(&mut self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.context.preflight_chunks(
            destination.len() as u64,
            (destination.len() as u64).max(1),
            1,
        )?;
        self.context.attempted_io();
        if let Some(profiler) = self.context.profiler() {
            profiler.measure(
                if self.phase == OperationPhase::AllocationScan {
                    crate::OperationEvent::AllocationRead
                } else {
                    crate::OperationEvent::SourceRead
                },
                destination.len() as u64,
                || self.source.read_exact_at(offset, destination),
            )?;
        } else {
            self.source.read_exact_at(offset, destination)?;
        }
        self.context.completed(destination.len() as u64);
        self.completed += destination.len() as u64;
        self.context
            .observe_phase(self.phase, self.completed, self.total)
    }
}
