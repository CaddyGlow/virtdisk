#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use std::sync::Arc;
use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt, ShrinkPolicy};
const C: u64 = 65536;
#[test]
fn native_shrink_and_regrow_clear_truncated_boundary_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let mut w = Qcow2Writer::create(&path, 2 * C).unwrap();
    w.write_all_at(0, &vec![9; (2 * C) as usize]).unwrap();
    assert!(w.resize(512, ShrinkPolicy::Reject).is_err());
    assert!(w.resize(512, ShrinkPolicy::RequireZero).is_err());
    assert_eq!(w.len(), 2 * C);
    w.resize(512, ShrinkPolicy::AllowDataLoss).unwrap();
    assert_eq!(w.len(), 512);
    w.resize(3 * C, ShrinkPolicy::Reject).unwrap();
    let mut bytes = vec![0; (3 * C) as usize];
    w.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes[..512], &[9; 512]);
    assert!(bytes[512..].iter().all(|b| *b == 0));
    w.flush().unwrap();
    drop(w);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
}
#[test]
fn grow_crosses_l1_coverage_and_zero_capacity_without_rewriting_image() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.qcow2");
    let mut w = Qcow2Writer::create(&path, 0).unwrap();
    w.resize(512, ShrinkPolicy::Reject).unwrap();
    w.write_all_at(0, &[7; 512]).unwrap();
    w.resize(512 * 1024 * 1024 + C, ShrinkPolicy::Reject)
        .unwrap();
    w.write_all_at(512 * 1024 * 1024, &[3; 512]).unwrap();
    w.flush().unwrap();
    drop(w);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    let mut bytes = [0; 512];
    disk.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [7; 512]);
    disk.read_exact_at(512 * 1024 * 1024, &mut bytes).unwrap();
    assert_eq!(bytes, [3; 512]);
}

#[test]
fn unsupported_ranges_and_zero_tail_policy_are_checked_before_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let mut w = Qcow2Writer::create(&path, 2 * C).unwrap();
    w.write_all_at(0, &[7; 512]).unwrap();
    w.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(w.resize(513, ShrinkPolicy::AllowDataLoss).is_err());
    assert!(
        w.resize(32 * 1024 * 1024 * 1024 + 512, ShrinkPolicy::Reject)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    w.resize(512, ShrinkPolicy::RequireZero).unwrap();
    w.resize(0, ShrinkPolicy::AllowDataLoss).unwrap();
    w.resize(C, ShrinkPolicy::Reject).unwrap();
    let mut bytes = vec![1; C as usize];
    w.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|b| *b == 0));
    drop(w);
    let parent = dir.path().join("base.raw");
    let child = dir.path().join("child.qcow2");
    std::fs::write(&parent, vec![9; C as usize]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &parent, "raw", C).unwrap();
    let parent_bytes = std::fs::read(&parent).unwrap();
    let mut w = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    w.resize(2 * C, ShrinkPolicy::Reject).unwrap();
    let mut bytes = vec![1; (2 * C) as usize];
    w.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes[..C as usize].iter().all(|byte| *byte == 9));
    assert!(bytes[C as usize..].iter().all(|byte| *byte == 0));
    assert_eq!(std::fs::read(&parent).unwrap(), parent_bytes);
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_checks_native_shrink_zero_regrowth_and_l1_growth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let raw = dir.path().join("oracle.raw");
    let mut w = Qcow2Writer::create(&path, 2 * C).unwrap();
    w.write_all_at(0, &vec![9; (2 * C) as usize]).unwrap();
    w.resize(512, ShrinkPolicy::AllowDataLoss).unwrap();
    drop(w);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let mut w = Qcow2Writer::open(&path).unwrap();

    w.resize(512 * 1024 * 1024 + C, ShrinkPolicy::Reject)
        .unwrap();
    w.write_all_at(512 * 1024 * 1024, &[3; 512]).unwrap();
    w.flush().unwrap();
    drop(w);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    let reader = RawDisk::open(&raw).unwrap();
    assert_eq!(reader.len(), 512 * 1024 * 1024 + C);
    let mut bytes = [1; 512];
    reader.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [9; 512]);
    reader.read_exact_at(512, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 512]);
    reader.read_exact_at(512 * 1024 * 1024, &mut bytes).unwrap();
    assert_eq!(bytes, [3; 512]);
}
