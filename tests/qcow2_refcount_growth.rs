#![cfg(target_os = "linux")]

use std::{fs::File, os::unix::fs::FileExt, path::Path, sync::Arc};
use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt};

const CLUSTER: u64 = 65536;
const BOUNDARY: u64 = CLUSTER * 32768;
const MASK: u64 = 0x00ff_ffff_ffff_fe00;

fn number(path: &Path, offset: u64) -> u64 {
    let mut bytes = [0; 8];
    File::open(path)
        .unwrap()
        .read_exact_at(&mut bytes, offset)
        .unwrap();
    u64::from_be_bytes(bytes)
}

fn reference(path: &Path, offset: u64) -> u16 {
    let table = number(path, 48);
    let index = offset / CLUSTER;
    let block = number(path, table + index / 32768 * 8);
    assert_ne!(block, 0);
    let mut bytes = [0; 2];
    File::open(path)
        .unwrap()
        .read_exact_at(&mut bytes, block + index % 32768 * 2)
        .unwrap();
    u16::from_be_bytes(bytes)
}

fn logical(path: &Path, snapshot: bool) -> Vec<u8> {
    let disk = Arc::new(Qcow2::open(Arc::new(RawDisk::open(path).unwrap())).unwrap());
    disk.validate_active_mapping().unwrap();
    let source: Arc<dyn ReadAt> = if snapshot {
        Arc::new(disk.open_snapshot(b"saved").unwrap())
    } else {
        disk
    };
    let mut bytes = vec![0; source.len() as usize];
    source.read_exact_at(0, &mut bytes).unwrap();
    bytes
}

fn exercise(path: &Path, revert: bool) -> (Vec<u8>, Vec<u8>) {
    let mut saved = vec![0; (2 * CLUSTER) as usize];
    saved[500..512].fill(17);
    let mut writer = Qcow2Writer::create_sparse(path, 2 * CLUSTER).unwrap();
    writer.write_all_at(500, &[17; 12]).unwrap();
    let original_l2 = number(path, number(path, 40)) & MASK;
    let original_payload = number(path, original_l2) & MASK;
    writer.create_snapshot(b"saved", b"saved").unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(reference(path, original_l2), 2);
    assert_eq!(reference(path, original_payload), 2);
    // Sparse, unreferenced tail clusters need no refcounts. The next payload
    // fits the old block; cloning the shared L2 crosses its coverage boundary.
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(BOUNDARY - CLUSTER)
        .unwrap();
    let writer = Qcow2Writer::open(path).unwrap();
    writer.write_all_at(505, &[61; 2]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let table = number(path, 48);
    let new_block = number(path, table + 8);
    let new_l2 = number(path, number(path, 40)) & MASK;
    let new_payload = number(path, new_l2) & MASK;
    assert_eq!(new_payload, BOUNDARY - CLUSTER);
    assert_eq!(new_l2, BOUNDARY);
    assert_eq!(new_block, BOUNDARY + CLUSTER);
    for offset in [
        original_l2,
        original_payload,
        new_l2,
        new_payload,
        new_block,
    ] {
        assert_eq!(reference(path, offset), 1, "reference at {offset}");
    }
    assert_eq!(number(path, table + 16), 0);
    let mut active = saved.clone();
    active[505..507].fill(61);
    assert_eq!(logical(path, false), active);
    assert_eq!(logical(path, true), saved);
    // Deletion must release the old shared mappings without losing the newly
    // allocated block or its self-reference.
    let mut writer = Qcow2Writer::open(path).unwrap();
    if revert {
        writer.revert_snapshot(b"saved").unwrap();
        writer.flush().unwrap();
        drop(writer);
        assert_eq!(logical(path, false), saved);
        assert_eq!(logical(path, true), saved);
        assert_eq!(reference(path, original_payload), 2);
        assert_eq!(reference(path, new_payload), 0);
        assert_eq!(reference(path, new_l2), 0);
        assert_eq!(reference(path, new_block), 1);
        return (saved.clone(), saved);
    }
    writer.delete_snapshot(b"saved").unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(reference(path, original_l2), 0);
    assert_eq!(reference(path, original_payload), 0);
    assert_eq!(reference(path, new_block), 1);
    assert_eq!(logical(path, false), active);
    (saved, active)
}

#[test]
fn shared_mapping_cow_grows_refcount_block_and_lifecycle_releases_old_refs() {
    let dir = tempfile::tempdir().unwrap();
    exercise(&dir.path().join("boundary.qcow2"), false);
}

#[test]
fn revert_after_refcount_block_growth_retains_saved_bytes_and_self_reference() {
    let dir = tempfile::tempdir().unwrap();
    exercise(&dir.path().join("revert.qcow2"), true);
}

#[test]
#[ignore = "requires independent qemu-img oracle; hashes a sparse 2 GiB container"]
fn qemu_accepts_boundary_refcount_growth_and_exact_logical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boundary.qcow2");
    let (_, active) = exercise(&path, false);
    let check = std::process::Command::new("qemu-img")
        .args(["check", "-f", "qcow2"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let raw = dir.path().join("oracle.raw");
    let convert = std::process::Command::new("qemu-img")
        .args(["convert", "-f", "qcow2", "-O", "raw"])
        .arg(&path)
        .arg(&raw)
        .output()
        .unwrap();
    assert!(
        convert.status.success(),
        "{}",
        String::from_utf8_lossy(&convert.stderr)
    );
    assert_eq!(std::fs::read(raw).unwrap(), active);
}
