#![cfg(feature = "std")]
use std::sync::Arc;
use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt};
#[cfg(target_os = "linux")]
#[path = "../src/test_sync.rs"]
mod process_boundary;

#[test]
fn writes_cross_clusters_flush_and_reopen() {
    #[cfg(target_os = "linux")]
    let _process_boundary = process_boundary::writer_test();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let writer = Qcow2Writer::create(&path, 131072).unwrap();
    assert_eq!(writer.len(), 131072);
    writer.write_all_at(65532, &[7; 12]).unwrap();
    writer.write_zeroes(65535, 5).unwrap();
    let mut actual = [0; 12];
    writer.read_exact_at(65532, &mut actual).unwrap();
    assert_eq!(actual, [7, 7, 7, 0, 0, 0, 0, 0, 7, 7, 7, 7]);
    assert!(writer.write_all_at(131071, &[9, 9]).is_err());
    assert!(writer.write_zeroes(u64::MAX, 2).is_err());
    writer.flush().unwrap();
    drop(writer);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    disk.read_exact_at(65532, &mut actual).unwrap();
    assert_eq!(actual, [7, 7, 7, 0, 0, 0, 0, 0, 7, 7, 7, 7]);
    let writer = Qcow2Writer::open(&path).unwrap();
    writer.write_all_at(0, &[3; 512]).unwrap();
}

#[test]
fn exclusive_handles_and_creation_never_overwrite() {
    #[cfg(target_os = "linux")]
    let _process_boundary = process_boundary::writer_test();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let writer = Qcow2Writer::create(&path, 65536).unwrap();
    assert!(Qcow2Writer::create(&path, 65536).is_err());
    assert!(Qcow2Writer::open(&path).is_err());
    drop(writer);
    Qcow2Writer::open(&path).expect("writer lock must be released after its final handle drops");
}

#[test]
fn sparse_profile_is_rejected_without_mutation() {
    #[cfg(target_os = "linux")]
    let _process_boundary = process_boundary::writer_test();
    use virtdisk::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    drop(Qcow2Writer::create(&path, 65536).unwrap());
    // L2 begins at cluster 4 for this layout. Clear its payload mapping.
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(4 * 65536)).unwrap();
    file.write_all(&0u64.to_be_bytes()).unwrap();
    drop(file);
    let before = std::fs::read(&path).unwrap();
    assert!(Qcow2Writer::open(&path).is_err());
    assert!(std::fs::read(&path).unwrap() == before);
}

#[test]
fn unsupported_headers_fail_before_mutation() {
    #[cfg(target_os = "linux")]
    let _process_boundary = process_boundary::writer_test();
    use virtdisk::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    for (offset, bytes) in [
        (72, 1u64.to_be_bytes().to_vec()),
        (96, 3u32.to_be_bytes().to_vec()),
        (60, 1u32.to_be_bytes().to_vec()),
    ] {
        let path = dir.path().join(format!("disk-{offset}.qcow2"));
        drop(Qcow2Writer::create(&path, 65536).unwrap());
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&bytes).unwrap();
        drop(file);
        let before = std::fs::read(&path).unwrap();
        assert!(Qcow2Writer::open(&path).is_err());
        assert!(std::fs::read(path).unwrap() == before);
    }
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
#[cfg(target_os = "linux")]
fn qemu_validates_written_payload_and_sparse_allocation() {
    #[cfg(target_os = "linux")]
    let _process_boundary = process_boundary::subprocess_test();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let raw = dir.path().join("disk.raw");
    let sparse = dir.path().join("sparse.qcow2");
    let writer = Qcow2Writer::create(&path, 131072).unwrap();
    let expected: Vec<u8> = (0..131072).map(|i| (i % 251) as u8).collect();
    writer.write_all_at(0, &expected).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let command = |args: &[&str], path: &std::path::Path| {
        std::process::Command::new("qemu-img")
            .args(args)
            .arg(path)
            .status()
            .unwrap()
    };
    assert!(command(&["check", "-f", "qcow2"], &path).success());
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert!(std::fs::read(raw).unwrap() == expected);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["create", "-f", "qcow2", "-o", "cluster_size=65536"])
            .arg(&sparse)
            .arg("131072")
            .status()
            .unwrap()
            .success()
    );
    let writer = Qcow2Writer::open(&sparse).unwrap();
    writer.write_all_at(65534, &[7; 4]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert!(command(&["check", "-f", "qcow2"], &sparse).success());
}

#[test]
#[cfg(target_os = "linux")]
fn valid_shared_payload_copy_on_write_preserves_other_guest_cluster() {
    #[cfg(target_os = "linux")]
    let _process_boundary = process_boundary::writer_test();
    use virtdisk::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.qcow2");
    drop(Qcow2Writer::create(&path, 131072).unwrap());
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(4 * 65536)).unwrap();
    file.write_all(&(5u64 * 65536).to_be_bytes()).unwrap();
    file.write_all(&(5u64 * 65536).to_be_bytes()).unwrap();
    file.seek(SeekFrom::Start(3 * 65536 + 5 * 2)).unwrap();
    file.write_all(&2u16.to_be_bytes()).unwrap();
    file.write_all(&0u16.to_be_bytes()).unwrap();
    drop(file);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    drop(disk);
    let writer = Qcow2Writer::open(&path).unwrap();
    writer.write_all_at(0, &[7; 512]).unwrap();
    let mut actual = [9; 512];
    writer.read_exact_at(65536, &mut actual).unwrap();
    assert_eq!(actual, [0; 512]);
    writer.flush().unwrap();
    drop(writer);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    disk.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, [7; 512]);
}

#[test]
#[cfg(target_os = "linux")]
fn sparse_allocation_creates_private_mapping_and_preserves_neighbor_zeroes() {
    #[cfg(target_os = "linux")]
    let _process_boundary = process_boundary::writer_test();
    use virtdisk::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sparse-native.qcow2");
    drop(Qcow2Writer::create(&path, 131072).unwrap());
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(65536)).unwrap();
    file.write_all(&0u64.to_be_bytes()).unwrap();
    file.seek(SeekFrom::Start(3 * 65536 + 4 * 2)).unwrap();
    file.write_all(&[0; 6]).unwrap();
    drop(file);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    drop(disk);
    let writer = Qcow2Writer::open(&path).unwrap();
    writer.write_all_at(65534, &[7; 4]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    let mut data = [9; 8];
    disk.read_exact_at(65532, &mut data).unwrap();
    assert_eq!(data, [0, 0, 7, 7, 7, 7, 0, 0]);
}
