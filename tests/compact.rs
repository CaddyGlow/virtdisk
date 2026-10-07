use std::sync::Arc;
use virtdisk::{Image, ImageFormat, Qcow2, RawDisk, compact_image, create_sparse_qcow2};

#[test]
fn sparse_qcow_export_and_compaction_preserve_content_and_reduce_container_length() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source.raw");
    let mut bytes = vec![0; 5 * 65536 + 512];
    bytes[65539..65543].copy_from_slice(&[1, 2, 3, 4]);
    bytes[5 * 65536] = 9;
    std::fs::write(&raw, &bytes).unwrap();
    let source = RawDisk::open(&raw).unwrap();
    let full = directory.path().join("full.qcow2");
    virtdisk::create_qcow2(&full, &source).unwrap();
    let sparse = directory.path().join("sparse.qcow2");
    create_sparse_qcow2(&sparse, &source).unwrap();
    let reader = Qcow2::open(Arc::new(RawDisk::open(&sparse).unwrap())).unwrap();
    reader.validate_active_mapping().unwrap();
    assert!(virtdisk::compare_images(&source, &reader).unwrap());
    assert!(std::fs::metadata(&sparse).unwrap().len() < std::fs::metadata(&full).unwrap().len());
    let compacted = directory.path().join("compacted.qcow2");
    compact_image(
        &Image::open(&full, Some(ImageFormat::Qcow2)).unwrap(),
        &compacted,
        ImageFormat::Qcow2,
    )
    .unwrap();
    assert!(
        virtdisk::compare_images(
            &source,
            &Image::open(&compacted, Some(ImageFormat::Qcow2)).unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        std::fs::metadata(compacted).unwrap().len(),
        std::fs::metadata(sparse).unwrap().len()
    );
    assert!(compact_image(&source, &full, ImageFormat::Raw).is_err());
}

#[test]
fn compaction_across_native_profiles_preserves_zero_and_partial_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source.raw");
    let mut bytes = vec![0; 1048576 + 512];
    bytes[10] = 7;
    std::fs::write(&raw, bytes).unwrap();
    let source = RawDisk::open(raw).unwrap();
    for format in [
        ImageFormat::Raw,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let output = directory.path().join(format!("compact-{format:?}"));
        compact_image(&source, &output, format).unwrap();
        assert!(
            virtdisk::compare_images(&source, &Image::open(output, Some(format)).unwrap()).unwrap()
        );
    }
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_accepts_sparse_compacted_qcow2() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source.raw");
    let mut bytes = vec![0; 131584];
    bytes[65537] = 11;
    std::fs::write(&raw, &bytes).unwrap();
    let output = directory.path().join("sparse.qcow2");
    create_sparse_qcow2(&output, &RawDisk::open(&raw).unwrap()).unwrap();
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&output)
            .status()
            .unwrap()
            .success()
    );
    let flat = directory.path().join("flat.raw");
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(output)
            .arg(&flat)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(std::fs::read(flat).unwrap(), bytes);
}

#[test]
fn cancelled_compaction_leaves_no_published_output_or_staging_files() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source.raw");
    std::fs::write(&raw, vec![7; 131072]).unwrap();
    let source = RawDisk::open(&raw).unwrap();
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let output = directory.path().join(format!("cancel-{format:?}"));
        let calls = AtomicUsize::new(0);
        let result = virtdisk::compact_image_with_cancel(&source, &output, format, &|| {
            calls.fetch_add(1, Ordering::Relaxed) > 1
        });
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    let output = directory.path().join("empty.raw");
    let empty = virtdisk::DiskView::new(Arc::new(source), 0, 0).unwrap();
    assert!(
        virtdisk::compact_image_with_cancel(&empty, &output, ImageFormat::Raw, &|| true).is_err()
    );
    assert!(!output.exists());
}
