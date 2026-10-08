#![cfg(feature = "std")]
// Serialize process-spawning oracle tests with lock-release assertions: a forked
// child can briefly retain an inherited locked descriptor before exec closes it.
static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
use std::{fs, sync::Arc};
use virtdisk::{RawDisk, ReadAt, Vmdk, VmdkWriter, create_vmdk};
#[test]
fn writes_cross_grain_zeroes_and_retains_exclusive_lock() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vmdk");
    let d = VmdkWriter::create(&image, 196608).unwrap();
    assert_eq!(d.len(), 196608);
    assert!(!d.is_empty());
    assert!(VmdkWriter::open(&image).is_err());
    d.write_all_at(65530, &[7; 20]).unwrap();
    let mut bytes = [0; 20];
    d.read_exact_at(65530, &mut bytes).unwrap();
    assert_eq!(bytes, [7; 20]);
    d.write_zeroes(65535, 4).unwrap();
    d.flush().unwrap();
    assert!(d.write_all_at(196608, &[1]).is_err());
    assert!(d.write_all_at(u64::MAX, &[1]).is_err());
    assert!(d.write_all_at(d.len(), &[]).is_ok());
    assert!(d.read_exact_at(d.len() + 1, &mut []).is_err());
    drop(d);
    let reopened = VmdkWriter::open(&image).unwrap();
    reopened.write_all_at(0, &[2; 4]).unwrap();
    reopened.flush().unwrap();
    drop(reopened);
    let reader = Vmdk::open(Arc::new(RawDisk::open(&image).unwrap())).unwrap();
    reader.read_exact_at(65530, &mut bytes).unwrap();
    assert_eq!(&bytes[5..9], &[0; 4]);
}
#[test]
fn sparse_images_open_without_mutation() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw");
    fs::write(&raw, vec![0; 65536]).unwrap();
    let image = dir.path().join("disk.vmdk");
    create_vmdk(&image, &RawDisk::open(&raw).unwrap()).unwrap();
    let before = fs::read(&image).unwrap();
    drop(VmdkWriter::open(&image).unwrap());
    assert_eq!(fs::read(image).unwrap(), before);
}

#[test]
fn alias_is_rejected_and_concurrent_ranges_remain_independent() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vmdk");
    let d = Arc::new(VmdkWriter::create(&image, 196608).unwrap());
    std::thread::scope(|scope| {
        for i in 0..8u64 {
            let d = d.clone();
            scope.spawn(move || {
                for _ in 0..10 {
                    d.write_all_at(i * 512, &[i as u8; 512]).unwrap();
                }
            });
        }
    });
    for i in 0..8u64 {
        let mut data = [0; 512];
        d.read_exact_at(i * 512, &mut data).unwrap();
        assert_eq!(data, [i as u8; 512]);
    }
    drop(d);
    let mut before = fs::read(&image).unwrap();
    let gd = u64::from_le_bytes(before[56..64].try_into().unwrap()) as usize * 512;
    let gt = u32::from_le_bytes(before[gd..gd + 4].try_into().unwrap()) as usize * 512;
    let first: [u8; 4] = before[gt..gt + 4].try_into().unwrap();
    before[gt + 4..gt + 8].copy_from_slice(&first);
    fs::write(&image, &before).unwrap();
    assert!(VmdkWriter::open(&image).is_err());
    assert_eq!(fs::read(image).unwrap(), before);
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_reads_payload_after_positional_writes() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vmdk");
    let raw = dir.path().join("raw");
    let d = VmdkWriter::create(&image, 196608 + 512).unwrap();
    let mut expected = vec![0; 196608 + 512];
    expected[65520..131100].fill(83);
    d.write_all_at(65520, &expected[65520..131100]).unwrap();
    d.write_zeroes(70000, 30).unwrap();
    expected[70000..70030].fill(0);
    d.flush().unwrap();
    drop(d);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vmdk", "-O", "raw"])
            .arg(&image)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(raw).unwrap(), expected);
}

#[test]
#[cfg(target_os = "linux")]
fn sparse_grains_allocate_zero_padded_private_payload_and_reopen() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("sparse.vmdk");
    let d = VmdkWriter::create_sparse(&image, 4 * 65536 + 512).unwrap();
    let initial = fs::metadata(&image).unwrap().len();
    d.write_all_at(65530, &[9; 20]).unwrap();
    d.write_all_at(4 * 65536, &[7; 512]).unwrap();
    d.write_zeroes(3 * 65536, 512).unwrap();
    d.flush().unwrap();
    drop(d);
    assert_eq!(fs::metadata(&image).unwrap().len(), initial + 3 * 65536);
    let d = VmdkWriter::open(&image).unwrap();
    let mut data = [1; 20];
    d.read_exact_at(65530, &mut data).unwrap();
    assert_eq!(data, [9; 20]);
    let mut zero = [1; 512];
    d.read_exact_at(3 * 65536, &mut zero).unwrap();
    assert_eq!(zero, [0; 512]);
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_redundant_grain_tables_remain_consistent_after_sparse_writes() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vmdk");
    let raw = dir.path().join("raw");
    assert!(
        std::process::Command::new("qemu-img")
            .args(["create", "-f", "vmdk"])
            .arg(&image)
            .arg("262656")
            .status()
            .unwrap()
            .success()
    );
    let original = fs::read(&image).unwrap();
    let d = VmdkWriter::open(&image).unwrap();
    let mut expected = vec![0; 262656];
    expected[65530..65550].fill(9);
    expected[262144..].fill(7);
    d.write_all_at(65530, &[9; 20]).unwrap();
    d.write_all_at(262144, &[7; 512]).unwrap();
    d.flush().unwrap();
    drop(d);
    let updated = fs::read(&image).unwrap();
    let descriptor = String::from_utf8_lossy(&original[512..21 * 512]);
    let updated_descriptor = String::from_utf8_lossy(&updated[512..21 * 512]);
    assert_ne!(
        descriptor.lines().find(|l| l.starts_with("CID=")),
        updated_descriptor.lines().find(|l| l.starts_with("CID="))
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "vmdk"])
            .arg(&image)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vmdk", "-O", "raw"])
            .arg(&image)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(raw).unwrap(), expected);
}

#[test]
fn cid_changes_before_nonempty_payload_mutation_and_after_flush_epoch() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vmdk");
    let d = VmdkWriter::create(&image, 65536).unwrap();
    let original = fs::read(&image).unwrap();
    d.write_all_at(0, &[]).unwrap();
    d.flush().unwrap();
    assert_eq!(fs::read(&image).unwrap(), original);
    d.write_all_at(0, &[1]).unwrap();
    d.flush().unwrap();
    let first = fs::read(&image).unwrap()[512..21 * 512].to_vec();
    d.write_zeroes(0, 1).unwrap();
    d.flush().unwrap();
    let second = fs::read(&image).unwrap()[512..21 * 512].to_vec();
    assert_ne!(first, second);
}

#[test]
#[cfg(target_os = "linux")]
fn missing_tables_aliases_and_corrupt_sidecars_fail_before_mutation() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vmdk");
    drop(VmdkWriter::create_sparse(&image, 65536).unwrap());
    let mut before = fs::read(&image).unwrap();
    let gd = u64::from_le_bytes(before[56..64].try_into().unwrap()) as usize * 512;
    before[gd..gd + 4].fill(0);
    fs::write(&image, &before).unwrap();
    assert!(VmdkWriter::open(&image).is_err());
    assert_eq!(fs::read(&image).unwrap(), before);
    let image = dir.path().join("alias.vmdk");
    drop(VmdkWriter::create_sparse(&image, 65536).unwrap());
    fs::hard_link(&image, dir.path().join("other.vmdk")).unwrap();
    let before = fs::read(&image).unwrap();
    assert!(VmdkWriter::open(&image).is_err());
    assert_eq!(fs::read(&image).unwrap(), before);
    let image = dir.path().join("journal.vmdk");
    drop(VmdkWriter::create_sparse(&image, 65536).unwrap());
    let before = fs::read(&image).unwrap();
    fs::write(dir.path().join("journal.vmdk.virtdisk-transaction"), b"bad").unwrap();
    assert!(VmdkWriter::open(&image).is_err());
    assert!(Vmdk::open(Arc::new(RawDisk::open(&image).unwrap())).is_err());
    assert_eq!(fs::read(&image).unwrap(), before);
}

#[test]
fn shorter_legacy_cid_keeps_descriptor_layout_and_becomes_fresh() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vmdk");
    drop(VmdkWriter::create(&image, 65536).unwrap());
    let mut bytes = fs::read(&image).unwrap();
    let descriptor = String::from_utf8(
        bytes[512..21 * 512]
            .iter()
            .copied()
            .take_while(|b| *b != 0)
            .collect(),
    )
    .unwrap();
    let old = descriptor
        .lines()
        .find(|line| line.starts_with("CID="))
        .unwrap();
    let descriptor = descriptor.replacen(old, "CID=1234567", 1);
    bytes[512..21 * 512].fill(0);
    bytes[512..512 + descriptor.len()].copy_from_slice(descriptor.as_bytes());
    fs::write(&image, bytes).unwrap();
    let d = VmdkWriter::open(&image).unwrap();
    d.write_all_at(0, &[1]).unwrap();
    d.flush().unwrap();
    let bytes = fs::read(&image).unwrap();
    let updated = String::from_utf8_lossy(&bytes[512..21 * 512]);
    let cid = updated
        .lines()
        .find(|line| line.starts_with("CID="))
        .unwrap();
    assert_eq!(cid.len(), 11);
    assert_ne!(cid, "CID=1234567");
}

#[test]
fn hosted_overlay_partial_writes_preserve_parent_and_zero_masks() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vmdk");
    let child = dir.path().join("child.vmdk");
    let parent = virtdisk::VmdkWriter::create(&base, 131584).unwrap();
    parent.write_all_at(0, &vec![23; 131584]).unwrap();
    parent.flush().unwrap();
    drop(parent);
    let before = std::fs::read(&base).unwrap();
    assert!(virtdisk::VmdkWriter::create_overlay(&child, &base, &[]).is_err());
    assert!(!child.exists());
    let writer =
        virtdisk::VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base)).unwrap();
    assert_eq!(
        virtdisk::RawWriter::open(&child).err().unwrap().kind(),
        virtdisk::io::ErrorKind::WouldBlock
    );
    let mut out = [0; 32];
    writer.read_exact_at(65524, &mut out).unwrap();
    assert_eq!(out, [23; 32]);
    writer.write_all_at(65530, &[91; 12]).unwrap();
    writer.write_zeroes(131080, 12).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(std::fs::read(&base).unwrap(), before);
    assert!(virtdisk::VmdkWriter::open(&child).is_err());
    let writer = virtdisk::VmdkWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    assert_eq!(
        virtdisk::RawWriter::open(&child).err().unwrap().kind(),
        virtdisk::io::ErrorKind::WouldBlock
    );
    writer.read_exact_at(65524, &mut out).unwrap();
    assert_eq!(&out[..6], &[23; 6]);
    assert_eq!(&out[6..18], &[91; 12]);
    assert_eq!(&out[18..], &[23; 14]);
    writer.read_exact_at(131072, &mut out).unwrap();
    assert_eq!(&out[..8], &[23; 8]);
    assert_eq!(&out[8..20], &[0; 12]);
    assert_eq!(&out[20..], &[23; 12]);
}

#[test]
#[ignore = "requires qemu-img independent writable backing oracle"]
fn qemu_reads_native_overlay_and_qemu_child_after_cow_writes() {
    let _serial = TEST_SERIAL.lock().unwrap();
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vmdk");
    let child = dir.path().join("child.vmdk");
    let parent = virtdisk::VmdkWriter::create(&base, 131072).unwrap();
    parent.write_all_at(0, &vec![61; 131072]).unwrap();
    parent.flush().unwrap();
    drop(parent);
    let mut expected = vec![61; 131072];
    for qemu_child in [false, true] {
        if child.exists() {
            std::fs::remove_file(&child).unwrap();
        }
        if qemu_child {
            assert!(
                Command::new("qemu-img")
                    .args(["create", "-f", "vmdk", "-F", "vmdk", "-b"])
                    .arg(&base)
                    .arg(&child)
                    .status()
                    .unwrap()
                    .success()
            );
        } else {
            drop(
                virtdisk::VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base))
                    .unwrap(),
            );
        }
        let writer = virtdisk::VmdkWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
        writer.write_all_at(65529, &[4; 23]).unwrap();
        writer.write_zeroes(20, 13).unwrap();
        writer.flush().unwrap();
        drop(writer);
        expected[65529..65552].fill(4);
        expected[20..33].fill(0);
        assert!(
            Command::new("qemu-img")
                .arg("check")
                .arg(&child)
                .status()
                .unwrap()
                .success()
        );
        let output = dir.path().join(if qemu_child {
            "qemu-child.raw"
        } else {
            "native-child.raw"
        });
        assert!(
            Command::new("qemu-img")
                .args(["convert", "-f", "vmdk", "-O", "raw"])
                .arg(&child)
                .arg(&output)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(std::fs::read(output).unwrap(), expected);
        let disk = virtdisk::Vmdk::open_chain(&child, std::slice::from_ref(&base)).unwrap();
        let mut out = vec![0; 131072];
        disk.read_exact_at(0, &mut out).unwrap();
        assert_eq!(out, expected);
    }
}

#[test]
fn parent_mismatch_and_explicit_zero_overlay_fail_closed_or_mask() {
    let _serial = TEST_SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vmdk");
    let child = dir.path().join("child.vmdk");
    let writer = virtdisk::VmdkWriter::create(&base, 65536).unwrap();
    writer.write_all_at(0, &vec![8; 65536]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    drop(virtdisk::VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base)).unwrap());
    let mut image = std::fs::read(&child).unwrap();
    image[8..12].copy_from_slice(&5u32.to_le_bytes());
    let gd = u64::from_le_bytes(image[56..64].try_into().unwrap()) as usize * 512;
    let gt = u32::from_le_bytes(image[gd..gd + 4].try_into().unwrap()) as usize * 512;
    image[gt..gt + 4].copy_from_slice(&1u32.to_le_bytes());
    std::fs::write(&child, image).unwrap();
    let writer = virtdisk::VmdkWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    writer.write_all_at(17, &[9; 8]).unwrap();
    let mut out = [1; 48];
    writer.read_exact_at(0, &mut out).unwrap();
    assert_eq!(&out[..17], &[0; 17]);
    assert_eq!(&out[17..25], &[9; 8]);
    assert_eq!(&out[25..], &[0; 23]);
    writer.flush().unwrap();
    drop(writer);
    let unchanged = std::fs::read(&child).unwrap();
    let parent = virtdisk::VmdkWriter::open(&base).unwrap();
    parent.write_all_at(0, &[10]).unwrap();
    parent.flush().unwrap();
    drop(parent);
    assert!(virtdisk::VmdkWriter::open_chain(&child, std::slice::from_ref(&base)).is_err());
    assert_eq!(std::fs::read(&child).unwrap(), unchanged);
}
