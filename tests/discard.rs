#![cfg(feature = "std")]
use virtdisk::{DiscardPolicy, DiscardResult, ImageOperation, InspectImage, RawWriter, WriteAt};

#[test]
#[cfg(target_os = "linux")]
fn common_vmdk_discard_releases_native_mapping_and_preserves_neighbors() {
    use virtdisk::{Image, ImageFormat, ImageWriter, ReadAt};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native.vmdk");
    let writer = ImageWriter::create_sparse(&path, ImageFormat::Vmdk, 131072).unwrap();
    writer.write_all_at(0, &vec![19; 131072]).unwrap();
    writer.flush().unwrap();
    let original = std::fs::read(&path).unwrap();
    assert_eq!(
        writer
            .discard(1, 65536, DiscardPolicy::RequireDeallocation)
            .unwrap_err()
            .kind(),
        virtdisk::io::ErrorKind::Unsupported
    );
    assert!(
        writer
            .discard(65536, 65537, DiscardPolicy::RequireDeallocation)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        writer
            .discard(0, 65536, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    let mut bytes = vec![0; 131072];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes[..65536].iter().all(|byte| *byte == 0));
    assert!(bytes[65536..].iter().all(|byte| *byte == 19));
    assert_eq!(
        writer
            .inspection()
            .capabilities
            .get(ImageOperation::Discard),
        virtdisk::Capability::Supported
    );
    writer.flush().unwrap();
    drop(writer);
    let reader = Image::open(&path, Some(ImageFormat::Vmdk)).unwrap();
    let mut reopened = vec![0; 131072];
    reader.read_exact_at(0, &mut reopened).unwrap();
    assert_eq!(reopened, bytes);
}

#[test]
fn raw_discard_preserves_capacity_neighbor_bytes_and_zeroes_unaligned_range() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.raw");
    let writer = RawWriter::create(&path, 16384).unwrap();
    writer.write_all_at(0, &[7; 16384]).unwrap();
    let outcome = writer
        .discard(100, 12100, DiscardPolicy::AllowZeroFallback)
        .unwrap();
    #[cfg(target_os = "linux")]
    assert_eq!(outcome, DiscardResult::Deallocated);
    #[cfg(not(target_os = "linux"))]
    assert_eq!(outcome, DiscardResult::Zeroed);
    writer.flush().unwrap();
    let mut bytes = vec![0; 16384];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes[..100], &[7; 100]);
    assert!(bytes[100..12200].iter().all(|b| *b == 0));
    assert!(bytes[12200..].iter().all(|b| *b == 7));
    assert_eq!(writer.len(), 16384);
    drop(writer);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn discard_checks_ranges_before_mutation_and_generic_zero_fallback_is_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.raw");
    let writer = RawWriter::create(path, 512).unwrap();
    writer.write_all_at(0, &[9; 512]).unwrap();
    assert!(
        writer
            .discard(511, 2, DiscardPolicy::AllowZeroFallback)
            .is_err()
    );
    assert!(
        writer
            .discard(u64::MAX, 1, DiscardPolicy::RequireDeallocation)
            .is_err()
    );
    assert!(
        writer
            .discard(513, 0, DiscardPolicy::AllowZeroFallback)
            .is_err()
    );
    let mut bytes = [0; 512];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [9; 512]);
    let generic: &dyn WriteAt = &writer;
    generic
        .discard(512, 0, DiscardPolicy::RequireDeallocation)
        .unwrap();
    generic
        .discard(0, 512, DiscardPolicy::AllowZeroFallback)
        .unwrap();
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 512]);
    #[cfg(target_os = "linux")]
    assert_eq!(
        writer
            .inspection()
            .capabilities
            .get(ImageOperation::Discard),
        virtdisk::Capability::Supported
    );
}

#[test]
fn generic_writer_without_native_discard_never_silently_zeroes_strict_request() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("image.vdi");
    let writer = virtdisk::VdiWriter::create_sparse(path, 1048576).unwrap();
    writer.write_all_at(0, &[6; 512]).unwrap();
    let generic: &dyn WriteAt = &writer;
    assert_eq!(
        generic
            .discard(0, 512, DiscardPolicy::RequireDeallocation)
            .unwrap_err()
            .kind(),
        virtdisk::io::ErrorKind::Unsupported
    );
    let mut bytes = [0; 512];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [6; 512]);
    assert_eq!(
        generic
            .discard(0, 512, DiscardPolicy::AllowZeroFallback)
            .unwrap(),
        DiscardResult::Zeroed
    );
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 512]);
}

#[test]
#[cfg(target_os = "linux")]
fn aligned_raw_hole_punch_releases_host_blocks_without_changing_capacity() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.raw");
    let writer = RawWriter::create(&path, 2 * 1048576).unwrap();
    writer.write_all_at(0, &vec![1; 2 * 1048576]).unwrap();
    writer.flush().unwrap();
    let before = std::fs::metadata(&path).unwrap().blocks();
    assert_eq!(
        writer
            .discard(0, 1048576, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    writer.flush().unwrap();
    let after = std::fs::metadata(&path).unwrap().blocks();
    assert!(after < before);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 2 * 1048576);
}

#[test]
fn generic_qcow_discard_masks_parent_and_explicit_fallback_handles_partial_clusters() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let child = directory.path().join("child.qcow2");
    std::fs::write(&base, vec![7; 131072]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", 131072).unwrap();
    let writer = virtdisk::Qcow2Writer::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    let generic: &dyn WriteAt = &writer;
    assert_eq!(
        generic
            .discard(0, 65536, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    assert!(
        generic
            .discard(65537, 10, DiscardPolicy::RequireDeallocation)
            .is_err()
    );
    assert_eq!(
        generic
            .discard(65537, 10, DiscardPolicy::AllowZeroFallback)
            .unwrap(),
        DiscardResult::Zeroed
    );
    let mut bytes = vec![0; 131072];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes[..65536].iter().all(|b| *b == 0));
    assert_eq!(bytes[65536], 7);
    assert!(bytes[65537..65547].iter().all(|b| *b == 0));
    assert!(bytes[65547..].iter().all(|b| *b == 7));
    assert_eq!(std::fs::read(base).unwrap(), vec![7; 131072]);
    assert_eq!(
        writer
            .inspection()
            .capabilities
            .get(ImageOperation::Discard),
        virtdisk::Capability::Supported
    );
}
