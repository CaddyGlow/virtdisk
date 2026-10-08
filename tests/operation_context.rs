#![cfg(feature = "std")]
use std::{
    ops::ControlFlow,
    sync::atomic::{AtomicU64, Ordering},
};
use virtdisk::io;
use virtdisk::{
    OperationCancelled, OperationContext, OperationLimitExceeded, OperationLimits,
    OperationProgress, OperationResource, RawWriter, ReadAt, WriteAt, copy_image_with_context,
};

struct Source {
    bytes: Vec<u8>,
    reads: AtomicU64,
}
impl Source {
    fn new(length: usize, byte: u8) -> Self {
        Self {
            bytes: vec![byte; length],
            reads: AtomicU64::new(0),
        }
    }
}
impl ReadAt for Source {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let start = offset as usize;
        bytes.copy_from_slice(&self.bytes[start..start + bytes.len()]);
        Ok(())
    }
}

#[test]
fn partial_final_chunk_has_exact_progress_and_usage() {
    let directory = tempfile::tempdir().unwrap();
    let output = RawWriter::create(directory.path().join("output"), 1025).unwrap();
    let source = Source::new(1025, 37);
    let limits = OperationLimits::default()
        .logical_bytes(1025)
        .io_operations(10)
        .scratch_bytes(256)
        .unwrap();
    let mut events = Vec::new();
    let usage = {
        let mut observer = |event: OperationProgress| {
            events.push(event);
            ControlFlow::Continue(())
        };
        let mut context = OperationContext::new(limits).with_observer(&mut observer);
        copy_image_with_context(&source, &output, &mut context).unwrap();
        context.usage()
    };
    assert_eq!(
        events
            .iter()
            .map(|event| event.completed_bytes)
            .collect::<Vec<_>>(),
        [0, 256, 512, 768, 1024, 1025]
    );
    assert!(events.iter().all(|event| event.total_bytes == 1025));
    assert_eq!(usage.logical_bytes, 1025);
    assert_eq!(usage.io_operations, 10);
    assert_eq!(usage.peak_scratch_bytes, 256);
    assert_eq!(source.reads.load(Ordering::Relaxed), 5);
    let mut bytes = vec![0; 1025];
    output.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, source.bytes);
}

#[test]
fn cumulative_budgets_refuse_whole_copy_before_io() {
    let directory = tempfile::tempdir().unwrap();
    let output = RawWriter::create(directory.path().join("output"), 1024).unwrap();
    let source = Source::new(1024, 37);
    let limits = OperationLimits::default()
        .logical_bytes(2048)
        .io_operations(6)
        .scratch_bytes(512)
        .unwrap();
    let mut context = OperationContext::new(limits);
    copy_image_with_context(&source, &output, &mut context).unwrap();
    let before = context.usage();
    let second = Source::new(1024, 9);
    let error = copy_image_with_context(&second, &output, &mut context).unwrap_err();
    let limit = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OperationLimitExceeded>()
        .unwrap();
    assert_eq!(limit.resource(), OperationResource::IoOperations);
    assert_eq!(limit.requested(), 8);
    assert_eq!(limit.limit(), 6);
    assert_eq!(second.reads.load(Ordering::Relaxed), 0);
    assert_eq!(context.usage(), before);
    let mut bytes = [0; 1024];
    output.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 1024]);
    let mut context = OperationContext::new(OperationLimits::default().logical_bytes(1023));
    let error = copy_image_with_context(&second, &output, &mut context).unwrap_err();
    assert_eq!(
        error
            .get_ref()
            .unwrap()
            .downcast_ref::<OperationLimitExceeded>()
            .unwrap()
            .resource(),
        OperationResource::LogicalBytes
    );
    assert_eq!(second.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn cancellation_retains_completed_prefix_and_never_flushes() {
    let directory = tempfile::tempdir().unwrap();
    let output = RawWriter::create(directory.path().join("output"), 1024).unwrap();
    let source = Source::new(1024, 19);
    let mut observer = |event: OperationProgress| {
        if event.completed_bytes == 512 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::new(OperationLimits::default().scratch_bytes(512).unwrap())
        .with_observer(&mut observer);
    let error = copy_image_with_context(&source, &output, &mut context).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert_eq!(context.usage().logical_bytes, 512);
    assert_eq!(context.usage().io_operations, 2);
    let mut bytes = [0; 1024];
    output.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes[..512], &[19; 512]);
    assert_eq!(&bytes[512..], &[0; 512]);
}

struct FailedWriter;
impl WriteAt for FailedWriter {
    fn len(&self) -> u64 {
        512
    }
    fn write_all_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "injected write failure",
        ))
    }
    fn flush(&self) -> io::Result<()> {
        panic!("copy must not flush")
    }
}
#[test]
fn failed_io_is_charged_without_reporting_completed_bytes() {
    let source = Source::new(512, 9);
    let mut context = OperationContext::default();
    let error = copy_image_with_context(&source, &FailedWriter, &mut context).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(context.usage().io_operations, 2);
    assert_eq!(context.usage().logical_bytes, 0);
}

struct Huge;
impl ReadAt for Huge {
    fn len(&self) -> u64 {
        u64::MAX
    }
    fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
        panic!("overflow must refuse before reading")
    }
}
impl WriteAt for Huge {
    fn len(&self) -> u64 {
        u64::MAX
    }
    fn write_all_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
        panic!("overflow must refuse before writing")
    }
    fn flush(&self) -> io::Result<()> {
        panic!("copy must not flush")
    }
}
#[test]
fn io_count_overflow_and_invalid_scratch_are_rejected() {
    assert!(OperationLimits::default().scratch_bytes(0).is_err());
    assert!(OperationLimits::default().scratch_bytes(131073).is_err());
    let mut context = OperationContext::new(OperationLimits::default().scratch_bytes(1).unwrap());
    let error = copy_image_with_context(&Huge, &Huge, &mut context).unwrap_err();
    let limit = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OperationLimitExceeded>()
        .unwrap();
    assert_eq!(limit.requested(), u128::from(u64::MAX) * 2);
    assert_eq!(context.usage(), Default::default());
}

#[test]
fn cancellation_before_first_io_and_after_final_chunk_has_explicit_accounting() {
    let directory = tempfile::tempdir().unwrap();
    let output = RawWriter::create(directory.path().join("output"), 512).unwrap();
    let source = Source::new(512, 23);
    let mut observer = |_: OperationProgress| ControlFlow::Break(());
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert!(copy_image_with_context(&source, &output, &mut context).is_err());
    assert_eq!(context.usage(), Default::default());
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
    let mut bytes = [1; 512];
    output.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 512]);
    let mut observer = |event: OperationProgress| {
        if event.completed_bytes == event.total_bytes {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    let error = copy_image_with_context(&source, &output, &mut context).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(context.usage().logical_bytes, 512);
    output.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [23; 512]);
}

#[test]
fn empty_copy_needs_no_budget_or_scratch_and_size_mismatch_never_notifies() {
    let directory = tempfile::tempdir().unwrap();
    let output = RawWriter::create(directory.path().join("output"), 0).unwrap();
    let source = Source::new(0, 0);
    let mut events = Vec::new();
    {
        let mut observer = |event: OperationProgress| {
            events.push(event);
            ControlFlow::Continue(())
        };
        let mut context =
            OperationContext::new(OperationLimits::default().logical_bytes(0).io_operations(0))
                .with_observer(&mut observer);
        copy_image_with_context(&source, &output, &mut context).unwrap();
        assert_eq!(context.usage(), Default::default());
        let mismatched = Source::new(1, 37);
        let error = copy_image_with_context(&mismatched, &output, &mut context).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(mismatched.reads.load(Ordering::Relaxed), 0);
    }
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].total_bytes, 0);
}

struct FailedReader;
impl ReadAt for FailedReader {
    fn len(&self) -> u64 {
        512
    }
    fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "injected read failure",
        ))
    }
}
#[test]
fn read_failure_never_attempts_the_write_or_advances_progress() {
    let mut events = Vec::new();
    {
        let mut observer = |event: OperationProgress| {
            events.push(event);
            ControlFlow::Continue(())
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let error =
            copy_image_with_context(&FailedReader, &FailedWriter, &mut context).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(context.usage().io_operations, 1);
        assert_eq!(context.usage().logical_bytes, 0);
    }
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completed_bytes, 0);
}

#[test]
fn hash_uses_shared_accounting_and_matches_known_sha256() {
    let source = Source {
        bytes: b"abc".to_vec(),
        reads: AtomicU64::new(0),
    };
    let mut context = OperationContext::new(
        OperationLimits::default()
            .logical_bytes(3)
            .io_operations(3)
            .scratch_bytes(1)
            .unwrap(),
    );
    let hash = virtdisk::hash_image_with_context(&source, &mut context).unwrap();
    assert_eq!(
        hash,
        [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad
        ]
    );
    assert_eq!(source.reads.load(Ordering::Relaxed), 3);
    assert_eq!(context.usage().io_operations, 3);
    assert_eq!(context.usage().logical_bytes, 3);
    assert!(virtdisk::hash_image_with_context(&source, &mut context).is_err());
    assert_eq!(source.reads.load(Ordering::Relaxed), 3);
}

#[test]
fn comparison_bounds_combined_scratch_and_stops_at_first_difference() {
    let left = Source::new(513, 23);
    let mut right = Source::new(513, 23);
    right.bytes[256] = 19;
    let mut context = OperationContext::new(OperationLimits::default().scratch_bytes(512).unwrap());
    assert!(!virtdisk::compare_images_with_context(&left, &right, &mut context).unwrap());
    assert_eq!(context.usage().logical_bytes, 512);
    assert_eq!(context.usage().io_operations, 4);
    assert_eq!(context.usage().peak_scratch_bytes, 512);
    assert_eq!(left.reads.load(Ordering::Relaxed), 2);
    let mut context = OperationContext::new(OperationLimits::default().scratch_bytes(1).unwrap());
    let error = virtdisk::compare_images_with_context(&left, &right, &mut context).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    let limit = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OperationLimitExceeded>()
        .unwrap();
    assert_eq!(limit.resource(), OperationResource::ScratchBytes);
    assert_eq!(limit.limit(), 1);
    assert_eq!(limit.requested(), 2);
    assert_eq!(left.reads.load(Ordering::Relaxed), 2);
}

#[test]
fn read_operations_cancel_between_complete_chunks() {
    let source = Source::new(513, 9);
    let mut observer = |event: OperationProgress| {
        if event.completed_bytes == 256 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::new(OperationLimits::default().scratch_bytes(256).unwrap())
        .with_observer(&mut observer);
    let error = virtdisk::hash_image_with_context(&source, &mut context).unwrap_err();
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert_eq!(context.usage().logical_bytes, 256);
    assert_eq!(context.usage().io_operations, 1);
    let right = Source::new(513, 9);
    let mut context = OperationContext::new(OperationLimits::default().scratch_bytes(512).unwrap())
        .with_observer(&mut observer);
    let error = virtdisk::compare_images_with_context(&source, &right, &mut context).unwrap_err();
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert_eq!(context.usage().logical_bytes, 256);
    assert_eq!(context.usage().io_operations, 2);
    assert_eq!(right.reads.load(Ordering::Relaxed), 1);
}

#[test]
fn legacy_cancel_callback_does_not_poll_after_completion() {
    let directory = tempfile::tempdir().unwrap();
    let output = RawWriter::create(directory.path().join("output"), 512).unwrap();
    let source = Source::new(512, 9);
    let mut checks = 0;
    virtdisk::copy_image_with_cancel(&source, &output, || {
        checks += 1;
        checks > 1
    })
    .unwrap();
    assert_eq!(checks, 1);
}
