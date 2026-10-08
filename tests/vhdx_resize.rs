#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use virtdisk::{ShrinkPolicy, VhdxWriter};
const M: u64 = 1 << 20;
#[test]
fn native_grow_shrink_and_regrow_preserve_retained_bytes_and_zero_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let mut disk = VhdxWriter::create(&path, M).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.resize(3 * M, ShrinkPolicy::Reject).unwrap();
    disk.write_all_at(2 * M, &[8]).unwrap();
    assert!(disk.resize(512, ShrinkPolicy::RequireZero).is_err());
    disk.resize(512, ShrinkPolicy::AllowDataLoss).unwrap();
    assert_eq!(disk.len(), 512);
    disk.resize(3 * M, ShrinkPolicy::Reject).unwrap();
    let mut out = [0];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [7]);
    disk.read_exact_at(2 * M, &mut out).unwrap();
    assert_eq!(out, [0]);
    disk.flush().unwrap();
    drop(disk);
    assert_eq!(VhdxWriter::open(&path).unwrap().len(), 3 * M);
}
#[test]
fn native_range_policy_and_bat_space_preflight_leave_uuid_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let mut disk = VhdxWriter::create(&path, 2 * M).unwrap();
    disk.write_all_at(M, &[9]).unwrap();
    disk.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    for (size, policy) in [
        (0, ShrinkPolicy::AllowDataLoss),
        (513, ShrinkPolicy::Reject),
        (M, ShrinkPolicy::Reject),
        (M, ShrinkPolicy::RequireZero),
        (129u64 << 30, ShrinkPolicy::Reject),
    ] {
        assert!(disk.resize(size, policy).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    disk.resize(2 * M, ShrinkPolicy::Reject).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn growth_zeroes_adversarial_hidden_padding_and_retains_exclusive_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let disk = VhdxWriter::create(&path, 512).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.flush().unwrap();
    drop(disk);
    let mut bytes = std::fs::read(&path).unwrap();
    let physical = u64::from_le_bytes(
        bytes[(2 * M) as usize..(2 * M + 8) as usize]
            .try_into()
            .unwrap(),
    ) & !0xfffff;
    bytes[(physical + 512) as usize..(physical + M) as usize].fill(9);
    std::fs::write(&path, bytes).unwrap();
    let mut disk = VhdxWriter::open(&path).unwrap();
    assert!(VhdxWriter::open(&path).is_err());
    disk.resize(M, ShrinkPolicy::Reject).unwrap();
    assert!(VhdxWriter::open(&path).is_err());
    let mut tail = vec![1; M as usize - 512];
    disk.read_exact_at(512, &mut tail).unwrap();
    assert!(tail.iter().all(|&b| b == 0));
}
fn meta(bytes: &[u8]) -> usize {
    u64::from_le_bytes(bytes[196672..196680].try_into().unwrap()) as usize
}
#[test]
fn logical_4096_sector_and_relocated_size_item_use_validated_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    drop(VhdxWriter::create(&path, 4096).unwrap());
    let mut bytes = std::fs::read(&path).unwrap();
    let metadata = meta(&bytes);
    bytes[metadata + 65584..metadata + 65588].copy_from_slice(&4096u32.to_le_bytes());
    let size = bytes[metadata + 65552..metadata + 65560].to_vec();
    bytes[metadata + 70000..metadata + 70008].copy_from_slice(&size);
    bytes[metadata + 80..metadata + 84].copy_from_slice(&70000u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    let mut disk = VhdxWriter::open(&path).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(disk.resize(512, ShrinkPolicy::AllowDataLoss).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    disk.resize(8192, ShrinkPolicy::Reject).unwrap();
    disk.flush().unwrap();
    drop(disk);
    assert_eq!(VhdxWriter::open(&path).unwrap().len(), 8192);
}
#[test]
fn growth_crosses_native_chunk_bitmap_slot_without_shifting_payload_owners() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let mut disk = VhdxWriter::create(&path, M).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.resize(4097 * M, ShrinkPolicy::Reject).unwrap();
    disk.write_all_at(4096 * M, &[8]).unwrap();
    disk.flush().unwrap();
    drop(disk);
    let disk = VhdxWriter::open(&path).unwrap();
    let mut out = [0];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [7]);
    disk.read_exact_at(4096 * M, &mut out).unwrap();
    assert_eq!(out, [8]);
}
#[test]
fn backed_and_fixed_capacity_changes_are_refused_without_mutation() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.vhdx");
    let child = dir.path().join("child.vhdx");
    drop(VhdxWriter::create(&parent, M).unwrap());
    let original = std::fs::read(&parent).unwrap();
    let mut disk = VhdxWriter::create_overlay(&child, &parent, &[]).unwrap();
    let before = std::fs::read(&child).unwrap();
    assert!(disk.resize(2 * M, ShrinkPolicy::Reject).is_err());
    assert_eq!(std::fs::read(&child).unwrap(), before);
    assert_eq!(std::fs::read(&parent).unwrap(), original);
    drop(disk);
    let fixed = dir.path().join("fixed.vhdx");
    let raw = dir.path().join("raw.img");
    std::fs::write(&raw, vec![7; M as usize]).unwrap();
    virtdisk::create_vhdx(
        &fixed,
        Arc::new(virtdisk::RawDisk::open(&raw).unwrap()).as_ref(),
    )
    .unwrap();
    let mut bytes = std::fs::read(&fixed).unwrap();
    let metadata = meta(&bytes);
    bytes[metadata + 65540..metadata + 65544].copy_from_slice(&1u32.to_le_bytes());
    std::fs::write(&fixed, &bytes).unwrap();
    let mut disk = VhdxWriter::open(&fixed).unwrap();
    assert!(disk.resize(2 * M, ShrinkPolicy::Reject).is_err());
    assert_eq!(std::fs::read(&fixed).unwrap(), bytes);
}

#[test]
#[ignore = "requires independent qemu-img native resize oracle"]
fn qemu_accepts_native_capacity_and_exact_payload_after_shrink_regrowth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let raw = dir.path().join("oracle.raw");
    let mut disk = VhdxWriter::create(&path, 512).unwrap();
    disk.write_all_at(0, &[7; 512]).unwrap();
    disk.resize(3 * M, ShrinkPolicy::Reject).unwrap();
    disk.write_all_at(2 * M, &[8]).unwrap();
    disk.resize(M + 512, ShrinkPolicy::AllowDataLoss).unwrap();
    disk.resize(3 * M, ShrinkPolicy::Reject).unwrap();
    disk.flush().unwrap();
    drop(disk);
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
            .arg(&path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    let bytes = std::fs::read(raw).unwrap();
    assert_eq!(bytes.len(), (3 * M) as usize);
    assert_eq!(&bytes[..512], &[7; 512]);
    assert!(bytes[512..].iter().all(|&b| b == 0));
}

#[test]
fn size_item_spanning_native_redo_sectors_is_refused_before_uuid_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    drop(VhdxWriter::create(&path, 512).unwrap());
    let mut bytes = std::fs::read(&path).unwrap();
    let metadata = meta(&bytes);
    let value = bytes[metadata + 65552..metadata + 65560].to_vec();
    bytes[metadata + 69628..metadata + 69636].copy_from_slice(&value);
    bytes[metadata + 80..metadata + 84].copy_from_slice(&69628u32.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    let mut disk = VhdxWriter::open(&path).unwrap();
    assert!(disk.resize(1024, ShrinkPolicy::Reject).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn require_zero_accepts_only_the_removed_logical_tail_and_preserves_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let mut disk = VhdxWriter::create(&path, 2 * M).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.resize(512, ShrinkPolicy::RequireZero).unwrap();
    disk.resize(2 * M, ShrinkPolicy::Reject).unwrap();
    let mut out = vec![0; (2 * M) as usize];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out[0], 7);
    assert!(out[1..].iter().all(|&b| b == 0));
}
