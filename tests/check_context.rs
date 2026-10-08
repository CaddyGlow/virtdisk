#![cfg(feature = "std")]
use std::{
    fs,
    ops::ControlFlow,
    sync::atomic::{AtomicU64, Ordering},
};
use virtdisk::io;
use virtdisk::{
    CheckOptions, ImageFormat, OperationCancelled, OperationContext, OperationLimits,
    OperationPhase, OperationProgress, ReadAt, check_image_with_context,
    check_payload_with_context,
};

struct Payload {
    reads: AtomicU64,
    fail_after: u64,
}
impl ReadAt for Payload {
    fn len(&self) -> u64 {
        1025
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if offset >= self.fail_after {
            return Err(virtdisk::ReadContext {
                container: Some("parent".into()),
                offset: Some(offset),
                ..Default::default()
            }
            .error(
                "payload check",
                io::Error::new(io::ErrorKind::UnexpectedEof, "unavailable payload"),
            ));
        }
        bytes.fill(37);
        Ok(())
    }
}

#[test]
fn payload_check_preflights_budget_and_reports_final_partial_chunk() {
    let source = Payload {
        reads: AtomicU64::new(0),
        fail_after: u64::MAX,
    };
    let mut context = OperationContext::new(OperationLimits::default().logical_bytes(1024));
    assert!(check_payload_with_context(&source, &mut context).is_err());
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
    let mut events = Vec::new();
    {
        let mut observer = |event: OperationProgress| {
            events.push(event);
            ControlFlow::Continue(())
        };
        let limits = OperationLimits::default()
            .logical_bytes(1025)
            .io_operations(5)
            .scratch_bytes(256)
            .unwrap();
        let mut context = OperationContext::new(limits).with_observer(&mut observer);
        assert_eq!(
            check_payload_with_context(&source, &mut context).unwrap(),
            1025
        );
        assert_eq!(context.usage().logical_bytes, 1025);
        assert_eq!(context.usage().io_operations, 5);
    }
    assert_eq!(
        events
            .iter()
            .map(|event| event.completed_bytes)
            .collect::<Vec<_>>(),
        [0, 256, 512, 768, 1024, 1025]
    );
    assert!(
        events
            .iter()
            .all(|event| event.phase == OperationPhase::PayloadValidation)
    );
}

#[test]
fn payload_failure_preserves_source_context_and_attempt_accounting() {
    let source = Payload {
        reads: AtomicU64::new(0),
        fail_after: 256,
    };
    let mut context = OperationContext::new(OperationLimits::default().scratch_bytes(256).unwrap());
    let error = check_payload_with_context(&source, &mut context).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    let detail = error
        .get_ref()
        .unwrap()
        .downcast_ref::<virtdisk::ReadError>()
        .unwrap();
    assert_eq!(detail.context.offset, Some(256));
    assert_eq!(context.usage().logical_bytes, 256);
    assert_eq!(context.usage().io_operations, 2);
}

#[test]
fn image_check_can_cancel_before_opening_or_between_payload_chunks() {
    let directory = tempfile::tempdir().unwrap();
    let absent = directory.path().join("absent");
    let mut observer = |_: OperationProgress| ControlFlow::Break(());
    let mut context = OperationContext::default().with_observer(&mut observer);
    let error = check_image_with_context(
        &absent,
        ImageFormat::Raw,
        &[],
        CheckOptions::default(),
        &mut context,
    )
    .unwrap_err();
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert!(!absent.exists());
    let path = directory.path().join("raw");
    fs::write(&path, [37; 1025]).unwrap();
    let mut observer = |event: OperationProgress| {
        if event.phase == OperationPhase::PayloadValidation && event.completed_bytes == 256 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::new(OperationLimits::default().scratch_bytes(256).unwrap())
        .with_observer(&mut observer);
    let error = check_image_with_context(
        &path,
        ImageFormat::Raw,
        &[],
        CheckOptions { payload: true },
        &mut context,
    )
    .unwrap_err();
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert_eq!(context.usage().logical_bytes, 256);
    assert_eq!(fs::read(&path).unwrap(), [37; 1025]);
}

#[test]
fn context_checks_all_formats_with_separate_metadata_and_payload_scope() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source");
    fs::write(&raw, [37; 65536]).unwrap();
    let source = virtdisk::RawDisk::open(&raw).unwrap();
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let path = directory.path().join(format!("{format:?}"));
        virtdisk::convert_image(&source, &path, format).unwrap();
        let before = fs::read(&path).unwrap();
        let mut events = Vec::new();
        {
            let mut observer = |event: OperationProgress| {
                events.push(event);
                ControlFlow::Continue(())
            };
            let limits = OperationLimits::default()
                .logical_bytes(65536)
                .io_operations(8)
                .scratch_bytes(8192)
                .unwrap();
            let mut context = OperationContext::new(limits).with_observer(&mut observer);
            let report = check_image_with_context(
                &path,
                format,
                &[],
                CheckOptions { payload: true },
                &mut context,
            )
            .unwrap();
            assert_eq!(report.payload_bytes_read, 65536);
            assert_eq!(context.usage().io_operations, 8);
            assert_eq!(context.usage().logical_bytes, 65536);
        }
        assert_eq!(events[0].phase, OperationPhase::MetadataValidation);
        assert_eq!(
            events.last().unwrap().phase,
            OperationPhase::PayloadValidation
        );
        assert_eq!(events.last().unwrap().completed_bytes, 65536);
        let mut context =
            OperationContext::new(OperationLimits::default().logical_bytes(0).io_operations(0));
        let report =
            check_image_with_context(&path, format, &[], CheckOptions::default(), &mut context)
                .unwrap();
        assert_eq!(report.payload_bytes_read, 0);
        assert_eq!(context.usage(), Default::default());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn qcow_metadata_cancellation_keeps_typed_error_and_image_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("image");
    drop(virtdisk::ImageWriter::create(&path, ImageFormat::Qcow2, 65536).unwrap());
    let before = fs::read(&path).unwrap();
    let mut calls = 0;
    let mut observer = |event: OperationProgress| {
        assert_eq!(event.phase, OperationPhase::MetadataValidation);
        calls += 1;
        if calls == 2 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    let error = check_image_with_context(
        &path,
        ImageFormat::Qcow2,
        &[],
        CheckOptions::default(),
        &mut context,
    )
    .unwrap_err();
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert_eq!(context.usage(), Default::default());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn authorized_chain_check_preserves_parent_and_child() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent");
    let child = directory.path().join("child");
    fs::write(&parent, [19; 65536]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &parent, "raw", 65536).unwrap();
    let before = fs::read(&child).unwrap();
    let mut context = OperationContext::default();
    assert!(
        check_image_with_context(
            &child,
            ImageFormat::Qcow2,
            &[],
            CheckOptions { payload: true },
            &mut context
        )
        .is_err()
    );
    let report = check_image_with_context(
        &child,
        ImageFormat::Qcow2,
        std::slice::from_ref(&parent),
        CheckOptions { payload: true },
        &mut context,
    )
    .unwrap();
    assert_eq!(report.payload_bytes_read, 65536);
    assert_eq!(fs::read(&child).unwrap(), before);
    assert_eq!(fs::read(&parent).unwrap(), [19; 65536]);
}

#[test]
fn final_payload_cancellation_returns_no_success_report_and_empty_sweep_needs_no_budget() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    fs::write(&path, [19; 512]).unwrap();
    let mut observer = |event: OperationProgress| {
        if event.phase == OperationPhase::PayloadValidation && event.completed_bytes == 512 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    let error = check_image_with_context(
        &path,
        ImageFormat::Raw,
        &[],
        CheckOptions { payload: true },
        &mut context,
    )
    .unwrap_err();
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert_eq!(context.usage().logical_bytes, 512);
    let empty = directory.path().join("empty");
    fs::write(&empty, []).unwrap();
    let source = virtdisk::RawDisk::open(empty).unwrap();
    let mut context =
        OperationContext::new(OperationLimits::default().logical_bytes(0).io_operations(0));
    assert_eq!(
        check_payload_with_context(&source, &mut context).unwrap(),
        0
    );
    assert_eq!(context.usage(), Default::default());
}
