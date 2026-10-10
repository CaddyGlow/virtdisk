#![cfg(feature = "std")]
use std::sync::atomic::{AtomicU64, Ordering};
use virtdisk::{
    ConversionPolicy, Image, ImageFormat, OperationContext, OperationEvent, OperationProfiler,
    ReadAt, convert_image_with_policy_and_context, io,
};

struct Source {
    bytes: Vec<u8>,
    reads: AtomicU64,
    fail_after: u64,
}
impl ReadAt for Source {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        if self.reads.fetch_add(1, Ordering::Relaxed) >= self.fail_after {
            return Err(io::Error::other("injected immutable source failure"));
        }
        buffer.copy_from_slice(&self.bytes[offset as usize..offset as usize + buffer.len()]);
        Ok(())
    }
}
fn source(nonzero: bool) -> Source {
    let mut bytes = vec![0; 3 * 1048576 + 512];
    if nonzero {
        bytes[..1048576].fill(71);
        bytes[2 * 1048576..].fill(39);
    }
    Source {
        bytes,
        reads: AtomicU64::new(0),
        fail_after: u64::MAX,
    }
}

#[test]
fn streaming_export_reads_each_logical_block_once_and_retains_verified_publication() {
    let directory = tempfile::tempdir().unwrap();
    for format in [ImageFormat::Vdi, ImageFormat::Vhdx] {
        for nonzero in [false, true] {
            let source = source(nonzero);
            for streaming in [false, true] {
                let output = directory
                    .path()
                    .join(format!("{format:?}-{nonzero}-{streaming}"));
                let profiler = OperationProfiler::default();
                let mut context = OperationContext::default().with_profiler(profiler.clone());
                if streaming {
                    context = candidate_context().with_profiler(profiler.clone());
                }
                convert_image_with_policy_and_context(
                    &source,
                    &output,
                    format,
                    ConversionPolicy::Sparse,
                    &mut context,
                )
                .unwrap();
                let image = Image::open(&output, Some(format)).unwrap();
                let mut bytes = vec![0; source.bytes.len()];
                image.read_exact_at(0, &mut bytes).unwrap();
                assert_eq!(bytes, source.bytes);
                if streaming && nonzero {
                    let container = std::fs::read(&output).unwrap();
                    let tail = &container[container.len() - 1048576..];
                    assert_eq!(&tail[..512], &[39; 512]);
                    assert!(tail[512..].iter().all(|byte| *byte == 0));
                }
                assert_eq!(profiler.metrics(OperationEvent::ByteVerification).calls, 1);
                if streaming {
                    assert_eq!(profiler.metrics(OperationEvent::AllocationRead).calls, 0);
                    assert_eq!(profiler.metrics(OperationEvent::SourceRead).calls, 4);
                    assert_eq!(
                        profiler.metrics(OperationEvent::SourceRead).bytes,
                        source.len()
                    );
                } else {
                    assert_eq!(
                        profiler.metrics(OperationEvent::AllocationRead).bytes,
                        source.len()
                    );
                }
            }
        }
    }
}

#[test]
fn insufficient_streaming_budget_retains_two_pass_fallback() {
    let directory = tempfile::tempdir().unwrap();
    for format in [ImageFormat::Vdi, ImageFormat::Vhdx] {
        let profiler = OperationProfiler::default();
        let mut context = OperationContext::default()
            .with_profiler(profiler.clone())
            .with_streaming_allocation(true);
        let source = source(true);
        convert_image_with_policy_and_context(
            &source,
            directory.path().join(format!("{format:?}")),
            format,
            ConversionPolicy::Sparse,
            &mut context,
        )
        .unwrap();
        assert_eq!(
            profiler.metrics(OperationEvent::AllocationRead).bytes,
            source.len()
        );
        assert!(profiler.metrics(OperationEvent::SourceRead).calls > 4);
    }
}

#[test]
fn streaming_source_failure_and_cancellation_leave_no_published_output() {
    let directory = tempfile::tempdir().unwrap();
    for format in [ImageFormat::Vdi, ImageFormat::Vhdx] {
        let mut source = source(true);
        source.fail_after = 1;
        let output = directory.path().join(format!("{format:?}-failed"));
        let mut context = candidate_context();
        assert!(
            convert_image_with_policy_and_context(
                &source,
                &output,
                format,
                ConversionPolicy::Sparse,
                &mut context
            )
            .is_err()
        );
        assert!(!output.exists());
        let source = self::source(true);
        let output = directory.path().join(format!("{format:?}-cancelled"));
        let mut observer = |event: virtdisk::OperationProgress| {
            if event.phase == virtdisk::OperationPhase::ImageExport && event.completed_bytes > 0 {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        let mut context = candidate_context().with_observer(&mut observer);
        assert!(
            convert_image_with_policy_and_context(
                &source,
                &output,
                format,
                ConversionPolicy::Sparse,
                &mut context
            )
            .is_err()
        );
        assert!(!output.exists());
    }
}

#[test]
#[ignore = "requires independent qemu-img decoder"]
fn qemu_decodes_streaming_vdi_and_vhdx_with_partial_final_block() {
    let directory = tempfile::tempdir().unwrap();
    let source = source(true);
    for format in [ImageFormat::Vdi, ImageFormat::Vhdx] {
        let output = directory.path().join(format!("{format:?}.img"));
        let decoded = directory.path().join(format!("{format:?}.raw"));
        let mut context = candidate_context();
        convert_image_with_policy_and_context(
            &source,
            &output,
            format,
            ConversionPolicy::Sparse,
            &mut context,
        )
        .unwrap();
        let status = std::process::Command::new("qemu-img")
            .arg("convert")
            .arg("-O")
            .arg("raw")
            .arg(&output)
            .arg(&decoded)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read(decoded).unwrap(), source.bytes);
    }
}

fn candidate_context<'a>() -> OperationContext<'a> {
    OperationContext::new(
        virtdisk::OperationLimits::default()
            .working_memory_bytes(2 * 1048576)
            .unwrap(),
    )
    .with_streaming_allocation(true)
}

#[test]
fn streaming_verification_budget_refusal_does_not_publish() {
    let directory = tempfile::tempdir().unwrap();
    for format in [ImageFormat::Vdi, ImageFormat::Vhdx] {
        let source = source(true);
        let output = directory.path().join(format!("{format:?}"));
        let limits = virtdisk::OperationLimits::default()
            .working_memory_bytes(2 * 1048576)
            .unwrap()
            .logical_bytes(source.len());
        let mut context = OperationContext::new(limits).with_streaming_allocation(true);
        let error = convert_image_with_policy_and_context(
            &source,
            &output,
            format,
            ConversionPolicy::Sparse,
            &mut context,
        )
        .unwrap_err();
        assert!(
            error
                .get_ref()
                .unwrap()
                .downcast_ref::<virtdisk::OperationLimitExceeded>()
                .is_some()
        );
        assert!(!output.exists());
        assert_eq!(context.usage().logical_bytes, source.len());
    }
}
