#![cfg(feature = "std")]
use virtdisk::{
    ConversionPolicy, ImageFormat, OperationContext, OperationEvent, OperationProfiler, ReadAt,
    convert_image_with_policy_and_context, io,
};
struct Source {
    fail: bool,
}
impl ReadAt for Source {
    fn len(&self) -> u64 {
        131584
    }
    fn read_exact_at(&self, _: u64, bytes: &mut [u8]) -> io::Result<()> {
        if self.fail {
            return Err(io::Error::other("injected source failure"));
        }
        bytes.fill(37);
        Ok(())
    }
}
#[test]
fn conversion_profile_counts_payload_metadata_and_verification_without_changing_budgets() {
    let directory = tempfile::tempdir().unwrap();
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vhdx,
        ImageFormat::Vmdk,
    ] {
        let profiler = OperationProfiler::default();
        let mut context = OperationContext::default().with_profiler(profiler.clone());
        let path = directory.path().join(format!("{format:?}"));
        convert_image_with_policy_and_context(
            &Source { fail: false },
            &path,
            format,
            ConversionPolicy::Sparse,
            &mut context,
        )
        .unwrap();
        assert!(profiler.metrics(OperationEvent::SourceRead).bytes >= 131584);
        assert!(profiler.metrics(OperationEvent::PayloadWrite).bytes >= 131584);
        if format == ImageFormat::Raw {
            assert_eq!(profiler.metrics(OperationEvent::MetadataWrite).bytes, 0);
        } else {
            assert!(profiler.metrics(OperationEvent::MetadataWrite).bytes > 0);
        }
        assert!(profiler.metrics(OperationEvent::Sync).calls > 0);
        assert_eq!(profiler.metrics(OperationEvent::ByteVerification).calls, 1);
        assert_eq!(profiler.metrics(OperationEvent::Publication).calls, 1);
        assert_eq!(profiler.metrics(OperationEvent::Publication).failures, 0);
        let mut plain = OperationContext::default();
        convert_image_with_policy_and_context(
            &Source { fail: false },
            path.with_extension("plain"),
            format,
            ConversionPolicy::Sparse,
            &mut plain,
        )
        .unwrap();
        assert_eq!(context.usage(), plain.usage());
        assert!(plain.profiler().is_none());
    }
}
#[test]
fn failed_source_attempt_is_recorded_and_output_is_not_published() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed.qcow2");
    let profiler = OperationProfiler::default();
    let mut context = OperationContext::default().with_profiler(profiler.clone());
    assert!(
        convert_image_with_policy_and_context(
            &Source { fail: true },
            &path,
            ImageFormat::Qcow2,
            ConversionPolicy::Sparse,
            &mut context
        )
        .is_err()
    );
    let reads = profiler.metrics(OperationEvent::AllocationRead);
    assert_eq!(reads.calls, 1);
    assert_eq!(reads.bytes, 65536);
    assert_eq!(reads.failures, 1);
    assert_eq!(profiler.metrics(OperationEvent::Publication).calls, 0);
    assert!(!path.exists());
}

#[test]
fn byte_mismatch_counts_as_failed_verification_without_publication() {
    struct ChangingSource(std::sync::atomic::AtomicU64);
    impl ReadAt for ChangingSource {
        fn len(&self) -> u64 {
            512
        }
        fn read_exact_at(&self, _: u64, bytes: &mut [u8]) -> io::Result<()> {
            bytes.fill(self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u8);
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mismatch.qcow2");
    let profiler = OperationProfiler::default();
    let mut context = OperationContext::default().with_profiler(profiler.clone());
    let source = ChangingSource(std::sync::atomic::AtomicU64::new(1));
    assert!(
        convert_image_with_policy_and_context(
            &source,
            &path,
            ImageFormat::Qcow2,
            ConversionPolicy::Default,
            &mut context
        )
        .is_err()
    );
    assert_eq!(
        profiler.metrics(OperationEvent::ByteVerification).failures,
        1
    );
    assert_eq!(profiler.metrics(OperationEvent::Publication).calls, 0);
    assert!(!path.exists());
}
