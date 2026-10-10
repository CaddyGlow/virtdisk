#![cfg(feature = "std")]
use virtdisk::{
    ConversionPolicy, ImageFormat, OperationContext, OperationEvent, OperationLimits,
    OperationProfiler, ReadAt, convert_image_with_policy_and_context, io,
};
struct Source(u64);
impl ReadAt for Source {
    fn len(&self) -> u64 {
        self.0
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = if ((offset + i as u64) / 65536).is_multiple_of(16) {
                41
            } else {
                0
            };
        }
        Ok(())
    }
}
#[test]
fn bounded_index_skips_zero_cluster_rereads_and_preserves_partial_final_cluster() {
    let directory = tempfile::tempdir().unwrap();
    let source = Source(2 * 1048576 + 512);
    let mut bytes = Vec::new();
    for enabled in [false, true] {
        let profiler = OperationProfiler::default();
        let mut context = OperationContext::new(
            OperationLimits::default()
                .working_memory_bytes(2 << 20)
                .unwrap(),
        )
        .with_streaming_allocation(enabled)
        .with_profiler(profiler.clone());
        let path = directory.path().join(format!("{enabled}.qcow2"));
        convert_image_with_policy_and_context(
            &source,
            &path,
            ImageFormat::Qcow2,
            ConversionPolicy::Sparse,
            &mut context,
        )
        .unwrap();
        assert_eq!(
            profiler.metrics(OperationEvent::AllocationRead).bytes,
            source.len()
        );
        bytes.push(profiler.metrics(OperationEvent::SourceRead).bytes);
        assert!(context.usage().peak_scratch_bytes <= 2 << 20);
        let image = virtdisk::Image::open(path, Some(ImageFormat::Qcow2)).unwrap();
        let mut tail = [0; 512];
        image.read_exact_at(2 * 1048576, &mut tail).unwrap();
        assert_eq!(tail, [41; 512]);
    }
    assert_eq!(bytes[0], source.len());
    assert_eq!(bytes[1], 2 * 65536 + 512);
}
