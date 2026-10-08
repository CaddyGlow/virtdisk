#![cfg(feature = "std")]
use std::{
    ops::ControlFlow,
    sync::atomic::{AtomicUsize, Ordering},
};
use virtdisk::io;
use virtdisk::{
    Image, ImageFormat, OperationCancelled, OperationContext, OperationLimits, OperationPhase,
    OperationProgress, ReadAt, compact_image_with_context, convert_image_with_context,
};
struct Bytes(Vec<u8>, AtomicUsize);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        self.1.fetch_add(1, Ordering::Relaxed);
        let start = offset as usize;
        out.copy_from_slice(&self.0[start..start + out.len()]);
        Ok(())
    }
}
const FORMATS: [ImageFormat; 5] = [
    ImageFormat::Raw,
    ImageFormat::Qcow2,
    ImageFormat::Vhdx,
    ImageFormat::Vdi,
    ImageFormat::Vmdk,
];
#[test]
fn all_formats_account_export_and_verification_before_publication() {
    for format in FORMATS {
        for compact in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("output");
            let source = Bytes(vec![37; 66048], AtomicUsize::new(0));
            let mut events = Vec::new();
            let usage = {
                let mut observer = |event: OperationProgress| {
                    events.push(event);
                    ControlFlow::Continue(())
                };
                let mut context = OperationContext::default().with_observer(&mut observer);
                if compact {
                    compact_image_with_context(&source, &path, format, &mut context)
                } else {
                    convert_image_with_context(&source, &path, format, &mut context)
                }
                .unwrap();
                context.usage()
            };
            let passes = if matches!(format, ImageFormat::Vdi | ImageFormat::Vhdx)
                || (compact && format == ImageFormat::Qcow2)
            {
                3
            } else {
                2
            };
            assert_eq!(usage.logical_bytes, source.len() * passes);
            let chunks = source.len().div_ceil(65536);
            let expected_io = if format == ImageFormat::Raw {
                chunks * 4
            } else {
                chunks * (passes + 1)
            };
            assert_eq!(usage.io_operations, expected_io);
            assert_eq!(source.1.load(Ordering::Relaxed) as u64, chunks * passes);
            assert!(
                events
                    .iter()
                    .any(|e| e.phase == OperationPhase::OutputVerification
                        && e.completed_bytes == source.len())
            );
            assert_eq!(events.last().unwrap().phase, OperationPhase::Publication);
            let image = Image::open(&path, Some(format)).unwrap();
            let mut bytes = vec![0; source.0.len()];
            image.read_exact_at(0, &mut bytes).unwrap();
            assert_eq!(bytes, source.0);
            assert_eq!(directory.path().read_dir().unwrap().count(), 1);
        }
    }
}
#[test]
fn cancellation_before_publication_removes_staging_for_every_format() {
    for format in FORMATS {
        for compact in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("output");
            let source = Bytes(vec![7; 512], AtomicUsize::new(0));
            let mut observer = |event: OperationProgress| {
                if event.phase == OperationPhase::Publication {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let mut context = OperationContext::default().with_observer(&mut observer);
            let error = if compact {
                compact_image_with_context(&source, &path, format, &mut context)
            } else {
                convert_image_with_context(&source, &path, format, &mut context)
            }
            .unwrap_err();
            assert!(error.get_ref().unwrap().is::<OperationCancelled>());
            assert!(!path.exists());
            assert_eq!(directory.path().read_dir().unwrap().count(), 0);
        }
    }
}
#[test]
fn exhausted_budget_prevents_publication_and_preserves_usage() {
    for format in FORMATS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let source = Bytes(vec![7; 512], AtomicUsize::new(0));
        let mut context = OperationContext::new(OperationLimits::default().logical_bytes(512));
        assert!(convert_image_with_context(&source, &path, format, &mut context).is_err());
        assert_eq!(context.usage().logical_bytes, 512);
        assert!(!path.exists());
        assert_eq!(directory.path().read_dir().unwrap().count(), 0);
    }
}

#[test]
fn native_scratch_refusal_precedes_callbacks_and_source_reads() {
    for format in FORMATS
        .into_iter()
        .filter(|format| *format != ImageFormat::Raw)
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let source = Bytes(vec![7; 512], AtomicUsize::new(0));
        let callbacks = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut observer = |_: OperationProgress| {
            callbacks.set(callbacks.get() + 1);
            ControlFlow::Continue(())
        };
        let limits = OperationLimits::default().scratch_bytes(65535).unwrap();
        let mut context = OperationContext::new(limits).with_observer(&mut observer);
        let error = convert_image_with_context(&source, &path, format, &mut context).unwrap_err();
        let quota = error
            .get_ref()
            .unwrap()
            .downcast_ref::<virtdisk::OperationLimitExceeded>()
            .unwrap();
        assert_eq!(quota.resource(), virtdisk::OperationResource::ScratchBytes);
        assert_eq!(callbacks.get(), 0);
        assert_eq!(source.1.load(Ordering::Relaxed), 0);
        assert_eq!(context.usage(), virtdisk::OperationUsage::default());
        assert_eq!(directory.path().read_dir().unwrap().count(), 0);
    }
}
#[test]
fn zero_blocks_only_charge_reads_actually_performed() {
    for format in FORMATS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let source = Bytes(vec![0; 512], AtomicUsize::new(0));
        let reads = if format == ImageFormat::Qcow2 { 4 } else { 3 };
        let bytes = if format == ImageFormat::Qcow2 {
            1536
        } else {
            1024
        };
        let limits = OperationLimits::default()
            .logical_bytes(bytes)
            .io_operations(reads);
        let mut context = OperationContext::new(limits);
        compact_image_with_context(&source, &path, format, &mut context).unwrap();
        assert_eq!(context.usage().logical_bytes, bytes);
        assert_eq!(context.usage().io_operations, reads);
    }
}
#[test]
fn cancellation_after_first_export_chunk_preserves_attempted_work() {
    for format in FORMATS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let source = Bytes(vec![7; 66048], AtomicUsize::new(0));
        let marker = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut observer = |event: OperationProgress| {
            if event.completed_bytes != 0 {
                marker.set(true);
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let error = convert_image_with_context(&source, &path, format, &mut context).unwrap_err();
        assert!(error.get_ref().unwrap().is::<OperationCancelled>());
        assert!(marker.get());
        assert_eq!(context.usage().logical_bytes, 65536);
        assert_eq!(source.1.load(Ordering::Relaxed), 1);
        assert!(!path.exists());
        assert_eq!(directory.path().read_dir().unwrap().count(), 0);
    }
}

#[test]
fn verification_cancellation_keeps_staged_output_unpublished() {
    for format in FORMATS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let source = Bytes(vec![7; 512], AtomicUsize::new(0));
        let mut observer = |event: OperationProgress| {
            if event.phase == OperationPhase::OutputVerification && event.completed_bytes > 0 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let error = convert_image_with_context(&source, &path, format, &mut context).unwrap_err();
        assert!(error.get_ref().unwrap().is::<OperationCancelled>());
        assert!(!path.exists());
        assert_eq!(directory.path().read_dir().unwrap().count(), 0);
    }
}
#[test]
fn failed_source_read_counts_attempt_without_completed_bytes() {
    struct Failing;
    impl ReadAt for Failing {
        fn len(&self) -> u64 {
            512
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "source fault"))
        }
    }
    for format in FORMATS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let mut context = OperationContext::default();
        let error = convert_image_with_context(&Failing, &path, format, &mut context).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(error.to_string(), "source fault");
        assert_eq!(context.usage().io_operations, 1);
        assert_eq!(context.usage().logical_bytes, 0);
        assert!(!path.exists());
        assert_eq!(directory.path().read_dir().unwrap().count(), 0);
    }
}
#[test]
fn raw_materialization_adapts_scratch_and_cumulative_budgets() {
    let directory = tempfile::tempdir().unwrap();
    let source = Bytes(vec![37; 512], AtomicUsize::new(0));
    let limits = OperationLimits::default()
        .logical_bytes(2048)
        .io_operations(24)
        .scratch_bytes(256)
        .unwrap();
    let mut context = OperationContext::new(limits);
    for index in 0..2 {
        convert_image_with_context(
            &source,
            directory.path().join(index.to_string()),
            ImageFormat::Raw,
            &mut context,
        )
        .unwrap();
    }
    assert_eq!(context.usage().logical_bytes, 2048);
    assert_eq!(context.usage().io_operations, 24);
    assert_eq!(context.usage().peak_scratch_bytes, 256);
    let error = convert_image_with_context(
        &source,
        directory.path().join("refused"),
        ImageFormat::Raw,
        &mut context,
    )
    .unwrap_err();
    assert!(
        error
            .get_ref()
            .unwrap()
            .is::<virtdisk::OperationLimitExceeded>()
    );
    assert_eq!(directory.path().read_dir().unwrap().count(), 2);
}
