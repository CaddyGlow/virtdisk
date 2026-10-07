#![cfg(target_os = "linux")]

use std::{
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};
use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt};
const CLUSTER: u64 = 65536;
const SECOND: u64 = CLUSTER * 8192;
fn shared_fixture(path: &Path) {
    let writer = Qcow2Writer::create(path, CLUSTER).unwrap();
    writer.write_all_at(0, &vec![9; CLUSTER as usize]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    for (offset, value) in [
        (24, SECOND + CLUSTER),
        (CLUSTER, 4 * CLUSTER),
        (CLUSTER + 8, 4 * CLUSTER),
        (4 * CLUSTER, 5 * CLUSTER),
    ] {
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&value.to_be_bytes()).unwrap();
    }
    file.seek(SeekFrom::Start(36)).unwrap();
    file.write_all(&2u32.to_be_bytes()).unwrap();
    file.seek(SeekFrom::Start(3 * CLUSTER + 4 * 2)).unwrap();
    file.write_all(&2u16.to_be_bytes()).unwrap();
    file.write_all(&2u16.to_be_bytes()).unwrap();
    drop(file);
    Qcow2::open(Arc::new(RawDisk::open(path).unwrap()))
        .unwrap()
        .validate_active_mapping()
        .unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn shared_l2_cow_preserves_other_l1_alias_and_exact_refcounts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.qcow2");
    shared_fixture(&path);
    let writer = Qcow2Writer::open(&path).unwrap();
    writer.write_all_at(100, &[7; 4]).unwrap();
    let mut actual = [0; 8];
    writer.read_exact_at(96, &mut actual).unwrap();
    assert_eq!(actual, [9, 9, 9, 9, 7, 7, 7, 7]);
    writer.read_exact_at(SECOND + 96, &mut actual).unwrap();
    assert_eq!(actual, [9; 8]);
    writer.write_all_at(SECOND + 200, &[8; 4]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    disk.read_exact_at(200, &mut actual).unwrap();
    assert_eq!(actual, [9; 8]);
    disk.read_exact_at(SECOND + 200, &mut actual).unwrap();
    assert_eq!(actual, [8, 8, 8, 8, 9, 9, 9, 9]);
}

#[test]
#[cfg(target_os = "linux")]
fn shared_l2_discard_does_not_mask_the_other_alias() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.qcow2");
    shared_fixture(&path);
    let writer = Qcow2Writer::open(&path).unwrap();
    writer.discard(0, CLUSTER).unwrap();
    let mut actual = [1; 8];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, [0; 8]);
    writer.read_exact_at(SECOND, &mut actual).unwrap();
    assert_eq!(actual, [9; 8]);
    writer.flush().unwrap();
    drop(writer);
    let disk = Qcow2::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    let stats = disk.validate_active_mapping().unwrap();
    assert_eq!(stats.data_descriptors, 1);
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
#[cfg(target_os = "linux")]
fn qemu_checks_shared_l2_before_and_after_cow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.qcow2");
    shared_fixture(&path);
    let check = || {
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&path)
            .status()
            .unwrap()
    };
    assert!(check().success());
    let writer = Qcow2Writer::open(&path).unwrap();
    writer.write_all_at(100, &[7; 4]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert!(check().success());
    let flat = dir.path().join("flat.raw");
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&path)
            .arg(&flat)
            .status()
            .unwrap()
            .success()
    );
    let oracle = RawDisk::open(flat).unwrap();
    let mut actual = [0; 8];
    oracle.read_exact_at(96, &mut actual).unwrap();
    assert_eq!(actual, [9, 9, 9, 9, 7, 7, 7, 7]);
    oracle.read_exact_at(SECOND + 96, &mut actual).unwrap();
    assert_eq!(actual, [9; 8]);
}
