#![cfg(feature = "std")]
use virtdisk::{
    ConversionPolicy, Image, ImageFormat, RawDisk, ReadAt, convert_image, convert_image_with_policy,
};

#[test]
fn sparse_conversion_preserves_logical_content_across_density_and_run_lengths() {
    let directory = tempfile::tempdir().unwrap();
    // Include a partial final cluster and both clustered and alternating payload.
    let cluster = 65536;
    for percent in [0, 1, 6, 25, 50, 100] {
        for run in [1, 4, 16] {
            let mut bytes = vec![0u8; 32 * cluster + 512];
            for (index, block) in bytes.chunks_mut(cluster).enumerate() {
                if ((index / run) * 37) % 100 < percent {
                    for (offset, byte) in block.iter_mut().enumerate() {
                        *byte = (offset.wrapping_mul(73).wrapping_add(index) % 255 + 1) as u8;
                    }
                }
            }
            let input = directory.path().join(format!("input-{percent}-{run}"));
            let output = directory.path().join(format!("output-{percent}-{run}"));
            std::fs::write(&input, &bytes).unwrap();
            let source = RawDisk::open(&input).unwrap();
            convert_image_with_policy(
                &source,
                &output,
                ImageFormat::Qcow2,
                ConversionPolicy::Sparse,
            )
            .unwrap();
            let image = Image::open(&output, Some(ImageFormat::Qcow2)).unwrap();
            assert_eq!(image.len(), bytes.len() as u64);
            let mut actual = vec![0; bytes.len()];
            image.read_exact_at(0, &mut actual).unwrap();
            assert_eq!(actual, bytes);
            assert!(
                convert_image_with_policy(
                    &source,
                    &output,
                    ImageFormat::Qcow2,
                    ConversionPolicy::Sparse
                )
                .is_err()
            );
        }
    }
}

#[test]
fn sparse_policy_keeps_default_dense_compatibility_and_sixteenth_payload_small() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input");
    let mut bytes = vec![0; 16 * 65536];
    bytes[..65536].fill(123);
    std::fs::write(&input, &bytes).unwrap();
    let source = RawDisk::open(&input).unwrap();
    let dense = directory.path().join("dense");
    let sparse = directory.path().join("sparse");
    convert_image(&source, &dense, ImageFormat::Qcow2).unwrap();
    convert_image_with_policy(
        &source,
        &sparse,
        ImageFormat::Qcow2,
        ConversionPolicy::Sparse,
    )
    .unwrap();
    assert!(std::fs::metadata(dense).unwrap().len() >= bytes.len() as u64);
    assert!(std::fs::metadata(sparse).unwrap().len() < 8 * 65536);
}

#[test]
fn sparse_conversion_cancellation_during_verification_never_publishes() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input");
    let output = directory.path().join("output");
    std::fs::write(&input, vec![0; 16 * 65536]).unwrap();
    let source = RawDisk::open(input).unwrap();
    let mut observer = |event: virtdisk::OperationProgress| {
        if event.phase == virtdisk::OperationPhase::OutputVerification {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    let mut context = virtdisk::OperationContext::default().with_observer(&mut observer);
    assert!(
        virtdisk::convert_image_with_policy_and_context(
            &source,
            &output,
            ImageFormat::Qcow2,
            ConversionPolicy::Sparse,
            &mut context,
        )
        .is_err()
    );
    assert!(!output.exists());
}
