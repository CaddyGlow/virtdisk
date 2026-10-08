#![cfg(feature = "std")]
#![cfg(target_os = "linux")]

use std::sync::Arc;
use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt, create_qcow2_overlay};
const CLUSTER: u64 = 65536;

#[test]
#[cfg(target_os = "linux")]
fn discard_releases_payload_refcounts_and_masks_authorized_parent() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.raw");
    let child = dir.path().join("child.qcow2");
    let base = vec![9; 3 * CLUSTER as usize];
    std::fs::write(&parent, &base).unwrap();
    create_qcow2_overlay(&child, &parent, "raw", 3 * CLUSTER).unwrap();
    let writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer
        .write_all_at(CLUSTER, &vec![7; CLUSTER as usize])
        .unwrap();
    writer.discard(0, 2 * CLUSTER).unwrap();
    let mut actual = vec![1; 3 * CLUSTER as usize];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert!(actual[..2 * CLUSTER as usize].iter().all(|byte| *byte == 0));
    assert!(actual[2 * CLUSTER as usize..].iter().all(|byte| *byte == 9));
    writer.flush().unwrap();
    drop(writer);
    let disk = Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    let stats = disk.validate_active_mapping().unwrap();
    assert_eq!(stats.data_descriptors, 0);
    let mut byte = [1];
    disk.read_exact_at(0, &mut byte).unwrap();
    assert_eq!(byte, [0]);
    drop(disk);
    let writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(100, &[3; 4]).unwrap();
    writer.read_exact_at(96, &mut actual[..8]).unwrap();
    assert_eq!(&actual[..8], &[0, 0, 0, 0, 3, 3, 3, 3]);
    assert!(std::fs::read(parent).unwrap() == base);
}

#[test]
#[cfg(target_os = "linux")]
fn discard_alignment_and_final_partial_cluster_are_checked_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let writer = Qcow2Writer::create(&path, CLUSTER + 512).unwrap();
    writer
        .write_all_at(0, &vec![7; (CLUSTER + 512) as usize])
        .unwrap();
    for (offset, length) in [(1, CLUSTER), (0, 512), (CLUSTER, 512)] {
        assert_eq!(
            writer.discard(offset, length).unwrap_err().kind(),
            virtdisk::io::ErrorKind::InvalidInput
        );
    }
    assert!(writer.discard(u64::MAX, CLUSTER).is_err());
    let mut actual = vec![0; (CLUSTER + 512) as usize];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert!(actual.iter().all(|byte| *byte == 7));
    writer.discard(0, CLUSTER).unwrap();
    writer.read_exact_at(0, &mut actual).unwrap();
    assert!(actual[..CLUSTER as usize].iter().all(|byte| *byte == 0));
    assert!(actual[CLUSTER as usize..].iter().all(|byte| *byte == 7));
    writer.flush().unwrap();
    drop(writer);
    Qcow2::open(Arc::new(RawDisk::open(path).unwrap()))
        .unwrap()
        .validate_active_mapping()
        .unwrap();
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
#[cfg(target_os = "linux")]
fn qemu_checks_and_reads_zero_masked_discard() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.raw");
    let child = dir.path().join("child.qcow2");
    let flat = dir.path().join("flat.raw");
    std::fs::write(&parent, vec![9; 2 * CLUSTER as usize]).unwrap();
    create_qcow2_overlay(&child, &parent, "raw", 2 * CLUSTER).unwrap();
    let writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(0, &vec![7; CLUSTER as usize]).unwrap();
    writer.discard(0, CLUSTER).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&child)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&child)
            .arg(&flat)
            .status()
            .unwrap()
            .success()
    );
    let actual = std::fs::read(flat).unwrap();
    assert!(actual[..CLUSTER as usize].iter().all(|byte| *byte == 0));
    assert!(actual[CLUSTER as usize..].iter().all(|byte| *byte == 9));
}
