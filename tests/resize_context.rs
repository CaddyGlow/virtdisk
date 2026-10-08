use std::{
    io,
    ops::ControlFlow,
    sync::atomic::{AtomicUsize, Ordering},
};
use virtdisk::{
    Image, ImageFormat, OperationCancelled, OperationContext, OperationLimits, OperationPhase,
    OperationProgress, ReadAt, ShrinkPolicy, resize_image_with_context,
};
struct Bytes(Vec<u8>, AtomicUsize);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.1.fetch_add(1, Ordering::Relaxed);
        let start = offset as usize;
        destination.copy_from_slice(&self.0[start..start + destination.len()]);
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
fn zero_tail_scan_export_and_verification_share_the_context() {
    for format in FORMATS {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output");
        let mut original = vec![0; 4096];
        original[..512].fill(37);
        let source = Bytes(original.clone(), AtomicUsize::new(0));
        let mut events = Vec::new();
        let usage = {
            let mut observer = |event: OperationProgress| {
                events.push(event);
                ControlFlow::Continue(())
            };
            let mut context = OperationContext::default().with_observer(&mut observer);
            resize_image_with_context(
                &source,
                &output,
                format,
                512,
                ShrinkPolicy::RequireZero,
                &mut context,
            )
            .unwrap();
            context.usage()
        };
        let passes = if matches!(format, ImageFormat::Vdi | ImageFormat::Vhdx) {
            3
        } else {
            2
        };
        assert_eq!(usage.logical_bytes, 3584 + passes * 512);
        assert!(
            events
                .iter()
                .any(|event| event.phase == OperationPhase::TailValidation
                    && event.completed_bytes == 3584
                    && event.total_bytes == 3584)
        );
        assert_eq!(events.last().unwrap().phase, OperationPhase::Publication);
        let image = Image::open(&output, Some(format)).unwrap();
        let mut actual = vec![0; 512];
        image.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, original[..512]);
        assert_eq!(source.0, original);
    }
}
#[test]
fn cancellation_and_quota_refusal_during_tail_validation_leave_no_output() {
    let directory = tempfile::tempdir().unwrap();
    let source = Bytes(vec![0; 131584], AtomicUsize::new(0));
    let output = directory.path().join("output");
    let mut observer = |event: OperationProgress| {
        if event.phase == OperationPhase::TailValidation && event.completed_bytes != 0 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    let error = resize_image_with_context(
        &source,
        &output,
        ImageFormat::Raw,
        512,
        ShrinkPolicy::RequireZero,
        &mut context,
    )
    .unwrap_err();
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert_eq!(context.usage().logical_bytes, 65536);
    assert_eq!(context.usage().io_operations, 1);
    assert_eq!(directory.path().read_dir().unwrap().count(), 0);
    let mut context = OperationContext::new(OperationLimits::default().logical_bytes(131071));
    assert!(
        resize_image_with_context(
            &source,
            &output,
            ImageFormat::Raw,
            512,
            ShrinkPolicy::RequireZero,
            &mut context
        )
        .is_err()
    );
    assert_eq!(context.usage(), virtdisk::OperationUsage::default());
    assert_eq!(source.1.load(Ordering::Relaxed), 1);
}
#[test]
fn growth_counts_synthetic_zero_ranges_and_preserves_original_bytes() {
    for format in FORMATS {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output");
        let source = Bytes(vec![37; 512], AtomicUsize::new(0));
        let mut context = OperationContext::default();
        resize_image_with_context(
            &source,
            &output,
            format,
            1024,
            ShrinkPolicy::Reject,
            &mut context,
        )
        .unwrap();
        let image = Image::open(&output, Some(format)).unwrap();
        let mut actual = vec![0; 1024];
        image.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(&actual[..512], &source.0);
        assert!(actual[512..].iter().all(|&byte| byte == 0));
        let passes = if matches!(format, ImageFormat::Vdi | ImageFormat::Vhdx) {
            3
        } else {
            2
        };
        assert_eq!(context.usage().logical_bytes, passes * 1024);
    }
}
#[test]
fn refused_shrink_policy_and_nonzero_tail_preserve_source_and_output_absence() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("output");
    let source = Bytes(vec![37; 1024], AtomicUsize::new(0));
    let mut context = OperationContext::default();
    assert!(
        resize_image_with_context(
            &source,
            &output,
            ImageFormat::Raw,
            512,
            ShrinkPolicy::Reject,
            &mut context
        )
        .is_err()
    );
    assert_eq!(context.usage(), virtdisk::OperationUsage::default());
    let error = resize_image_with_context(
        &source,
        &output,
        ImageFormat::Raw,
        512,
        ShrinkPolicy::RequireZero,
        &mut context,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(context.usage().logical_bytes, 512);
    assert_eq!(context.usage().io_operations, 1);
    assert_eq!(directory.path().read_dir().unwrap().count(), 0);
    resize_image_with_context(
        &source,
        &output,
        ImageFormat::Raw,
        512,
        ShrinkPolicy::AllowDataLoss,
        &mut context,
    )
    .unwrap();
    assert_eq!(context.usage().logical_bytes, 1536);
    assert_eq!(source.0, vec![37; 1024]);
}

#[test]
fn tail_read_failure_keeps_original_io_kind_and_attempted_call() {
    struct Failed;
    impl ReadAt for Failed {
        fn len(&self) -> u64 {
            1024
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tail read failed",
            ))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("output");
    let mut context = OperationContext::default();
    let error = resize_image_with_context(
        &Failed,
        &output,
        ImageFormat::Raw,
        512,
        ShrinkPolicy::RequireZero,
        &mut context,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(error.to_string(), "tail read failed");
    assert_eq!(context.usage().logical_bytes, 0);
    assert_eq!(context.usage().io_operations, 1);
    assert_eq!(directory.path().read_dir().unwrap().count(), 0);
}
#[test]
fn completely_synthetic_growth_reads_are_facade_work() {
    let directory = tempfile::tempdir().unwrap();
    let source = Bytes(vec![37; 512], AtomicUsize::new(0));
    let output = directory.path().join("output");
    let mut context = OperationContext::default();
    resize_image_with_context(
        &source,
        &output,
        ImageFormat::Raw,
        131584,
        ShrinkPolicy::Reject,
        &mut context,
    )
    .unwrap();
    assert_eq!(context.usage().logical_bytes, 2 * 131584);
    assert_eq!(context.usage().io_operations, 12);
    assert_eq!(source.1.load(Ordering::Relaxed), 2);
    let bytes = std::fs::read(output).unwrap();
    assert_eq!(&bytes[..512], &source.0);
    assert!(bytes[512..].iter().all(|&byte| byte == 0));
}
#[test]
fn resize_cancellation_before_publication_keeps_every_format_unpublished() {
    for format in FORMATS {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output");
        let source = Bytes(vec![0; 1024], AtomicUsize::new(0));
        let mut observer = |event: OperationProgress| {
            if event.phase == OperationPhase::Publication {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let error = resize_image_with_context(
            &source,
            &output,
            format,
            512,
            ShrinkPolicy::RequireZero,
            &mut context,
        )
        .unwrap_err();
        assert!(error.get_ref().unwrap().is::<OperationCancelled>());
        assert!(!output.exists());
        assert_eq!(directory.path().read_dir().unwrap().count(), 0);
    }
}
