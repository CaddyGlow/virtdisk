#![cfg(feature = "std")]
use std::sync::Arc;
use virtdisk::{RawDisk, ReadAt, Vhdx, VhdxWriter};
const M: usize = 1 << 20;
#[test]
fn sparse_create_partial_writes_and_reopen_preserve_untouched_zeroes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let writer = VhdxWriter::create(&path, (3 * M + 512) as u64).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 4 * M as u64);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    writer.write_all_at(M as u64 - 2, &[1, 2, 3, 4]).unwrap();
    writer.write_all_at(3 * M as u64 + 511, &[9]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Vhdx::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let mut bytes = vec![0; 3 * M + 512];
    disk.read_exact_at(0, &mut bytes).unwrap();
    let mut expected = vec![0; bytes.len()];
    expected[M - 2..M + 2].copy_from_slice(&[1, 2, 3, 4]);
    expected[3 * M + 511] = 9;
    assert_eq!(bytes, expected);
}
#[test]
fn zero_hole_writes_do_not_allocate_and_repeated_allocations_share_log_region() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    let writer = VhdxWriter::create(&path, 4 * M as u64).unwrap();
    writer.write_zeroes(0, 4 * M as u64).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 4 * M as u64);
    for index in 0..4 {
        writer
            .write_all_at(index * M as u64, &[(index % 251) as u8 + 1])
            .unwrap();
    }
    writer.flush().unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 9 * M as u64);
    let mut out = [0; 512];
    for index in 0..4 {
        writer.read_exact_at(index * M as u64, &mut out).unwrap();
        assert_eq!(out[0], (index % 251) as u8 + 1);
        assert!(out[1..].iter().all(|&v| v == 0));
    }
}
#[test]
fn logical_mapping_is_updated_eagerly_and_bat_sector_boundaries_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    let writer = VhdxWriter::create(&path, 514 * M as u64).unwrap();
    for index in [0, 511, 512, 513] {
        writer
            .write_all_at(index * M as u64 + 37, &[(index % 251) as u8 + 1])
            .unwrap();
        let mut b = [0];
        writer.read_exact_at(index * M as u64 + 37, &mut b).unwrap();
        assert_eq!(b, [(index % 251) as u8 + 1]);
    }
    writer.flush().unwrap();
    drop(writer);
    let disk = Vhdx::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    for index in [0, 511, 512, 513] {
        let mut b = [0];
        disk.read_exact_at(index * M as u64 + 37, &mut b).unwrap();
        assert_eq!(b, [(index % 251) as u8 + 1]);
    }
}
#[test]
#[ignore = "requires independent qemu-img"]
fn qemu_checks_and_converts_native_sparse_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    let raw = dir.path().join("raw");
    let writer = VhdxWriter::create(&path, (3 * M + 512) as u64).unwrap();
    writer.write_all_at(M as u64 - 2, &[1, 2, 3, 4]).unwrap();
    writer.write_all_at(3 * M as u64 + 511, &[9]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "vhdx"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vhdx", "-O", "raw"])
            .arg(path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    let mut expected = vec![0; 3 * M + 512];
    expected[M - 2..M + 2].copy_from_slice(&[1, 2, 3, 4]);
    expected[3 * M + 511] = 9;
    assert_eq!(std::fs::read(raw).unwrap(), expected);
}

#[test]
fn interleaved_bitmap_slots_and_concurrent_allocations_keep_distinct_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interleaved");
    let writer = Arc::new(VhdxWriter::create(&path, 4097 * M as u64).unwrap());
    std::thread::scope(|scope| {
        for index in [0u64, 511, 4095, 4096] {
            let writer = writer.clone();
            scope.spawn(move || {
                writer
                    .write_all_at(index * M as u64 + 37, &[(index % 251) as u8 + 1])
                    .unwrap()
            });
        }
    });
    writer.flush().unwrap();
    drop(writer);
    let disk = Vhdx::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    for index in [0u64, 511, 4095, 4096] {
        let mut out = [0; 2];
        disk.read_exact_at(index * M as u64 + 37, &mut out).unwrap();
        assert_eq!(out, [(index % 251) as u8 + 1, 0]);
    }
}
#[test]
fn sparse_creation_rejects_invalid_capacity_and_preserves_existing_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing");
    std::fs::write(&path, b"keep").unwrap();
    assert!(VhdxWriter::create(&path, 512).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"keep");
    for size in [0, 1, 513, u64::MAX, 10u64 << 40] {
        let path = dir.path().join(size.to_string());
        assert!(VhdxWriter::create(&path, size).is_err());
        assert!(!path.exists());
    }
}
