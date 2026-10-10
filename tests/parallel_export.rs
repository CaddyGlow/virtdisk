#![cfg(feature = "std")]
use std::sync::{
    Barrier,
    atomic::{AtomicUsize, Ordering},
};
use virtdisk::{
    ConversionPolicy, ImageFormat, OperationContext, OperationEvent, OperationLimits,
    OperationPhase, OperationProfiler, ReadAt, convert_image_with_policy_and_context, io,
};
struct Source {
    size: u64,
    calls: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
    barrier: Option<Barrier>,
    fail: bool,
    panic: bool,
}
impl ReadAt for Source {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        if let Some(barrier) = &self.barrier
            && call < 4
        {
            barrier.wait();
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        if self.panic && offset == 0 {
            panic!("injected preparation panic");
        }
        if self.fail && offset == 0 {
            return Err(io::Error::other("injected preparation read error"));
        }
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = ((offset + index as u64) % 251) as u8;
        }
        Ok(())
    }
}
fn source(barrier: bool) -> Source {
    Source {
        size: 1048576 + 512,
        calls: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        barrier: barrier.then(|| Barrier::new(4)),
        fail: false,
        panic: false,
    }
}
fn context<'a>() -> OperationContext<'a> {
    OperationContext::new(
        OperationLimits::default()
            .working_memory_bytes(1048576)
            .unwrap(),
    )
    .with_buffer_bytes(262144)
    .unwrap()
    .with_workers(4)
    .unwrap()
}
#[test]
fn bounded_parallel_preparation_verifies_partial_tail_and_joins_all_readers() {
    let directory = tempfile::tempdir().unwrap();
    let source = source(true);
    let mut context = context();
    convert_image_with_policy_and_context(
        &source,
        directory.path().join("out"),
        ImageFormat::Raw,
        ConversionPolicy::Sparse,
        &mut context,
    )
    .unwrap();
    assert_eq!(source.maximum.load(Ordering::SeqCst), 4);
    assert_eq!(source.active.load(Ordering::SeqCst), 0);
    assert!(context.usage().peak_scratch_bytes <= 1048576);
    assert_eq!(context.usage().logical_bytes, source.len() * 2);
}
#[test]
fn worker_errors_panics_and_coordinator_cancellation_never_publish() {
    let directory = tempfile::tempdir().unwrap();
    for fault in ["error", "panic", "cancel"] {
        let mut source = source(false);
        source.fail = fault == "error";
        source.panic = fault == "panic";
        let path = directory.path().join(fault);
        let profiler = OperationProfiler::default();
        let mut observer = |progress: virtdisk::OperationProgress| {
            if fault == "cancel"
                && progress.phase == OperationPhase::ImageExport
                && progress.completed_bytes > 0
            {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        let mut context = context()
            .with_observer(&mut observer)
            .with_profiler(profiler.clone());
        let error = convert_image_with_policy_and_context(
            &source,
            &path,
            ImageFormat::Raw,
            ConversionPolicy::Sparse,
            &mut context,
        )
        .unwrap_err();
        if fault == "error" {
            assert!(
                error
                    .to_string()
                    .contains("injected preparation read error")
            );
        }
        if fault == "cancel" {
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        }
        assert!(!path.exists());
        assert_eq!(source.active.load(Ordering::SeqCst), 0);
        assert_eq!(profiler.metrics(OperationEvent::Publication).calls, 0);
        assert_eq!(
            context.usage().io_operations,
            source.calls.load(Ordering::SeqCst) as u64 + u64::from(fault == "cancel")
        );
    }
}
#[test]
fn whole_operation_budget_refuses_parallel_io_before_source_reads() {
    let directory = tempfile::tempdir().unwrap();
    let source = source(false);
    let mut context = OperationContext::new(OperationLimits::default().io_operations(1))
        .with_workers(4)
        .unwrap();
    assert!(
        convert_image_with_policy_and_context(
            &source,
            directory.path().join("out"),
            ImageFormat::Raw,
            ConversionPolicy::Sparse,
            &mut context
        )
        .is_err()
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}
