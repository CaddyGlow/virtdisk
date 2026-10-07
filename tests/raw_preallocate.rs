use virtdisk::RawWriter;

#[test]
fn preallocation_bounds_do_not_change_capacity_or_payload() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk");
    let writer = RawWriter::create(&path, 65536).unwrap();
    writer.write_all_at(10, &[23; 32]).unwrap();
    assert!(writer.preallocate(65530, 7).is_err());
    assert!(writer.preallocate(u64::MAX, 1).is_err());
    assert!(writer.preallocate(65537, 0).is_err());
    writer.preallocate(65536, 0).unwrap();
    assert_eq!(writer.len(), 65536);
    let mut bytes = vec![0; 65536];
    writer.read_exact_at(0, &mut bytes).unwrap();
    let mut expected = vec![0; 65536];
    expected[10..42].fill(23);
    assert_eq!(bytes, expected);
}

#[cfg(target_os = "linux")]
#[test]
fn host_preallocation_reserves_holes_preserves_existing_bytes_and_retains_lock() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk");
    let writer = RawWriter::create(&path, 1048576).unwrap();
    use virtdisk::{Capability, ImageOperation, InspectImage};
    assert_eq!(
        writer
            .inspection()
            .capabilities
            .get(ImageOperation::Preallocate),
        Capability::Supported
    );
    writer.write_all_at(19, &[31; 32]).unwrap();
    let before = std::fs::metadata(&path).unwrap().blocks();
    writer.preallocate(0, 1048576).unwrap();
    writer.flush().unwrap();
    assert_eq!(writer.len(), 1048576);
    assert!(std::fs::metadata(&path).unwrap().blocks() > before);
    assert!(RawWriter::open(&path).is_err());
    let mut bytes = vec![0; 1048576];
    writer.read_exact_at(0, &mut bytes).unwrap();
    let mut expected = vec![0; 1048576];
    expected[19..51].fill(31);
    assert_eq!(bytes, expected);
    drop(writer);
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_preallocation_never_substitutes_zero_writes() {
    let directory = tempfile::tempdir().unwrap();
    let writer = RawWriter::create(directory.path().join("disk"), 512).unwrap();
    writer.write_all_at(0, &[41; 512]).unwrap();
    assert_eq!(
        writer.preallocate(0, 512).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    let mut bytes = [0; 512];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [41; 512]);
}
