#![cfg(target_os = "linux")]
use std::sync::Arc;
use virtdisk::{
    Capability, ImageOperation, InspectImage, Qcow2, Qcow2Writer, ReadAt, ShrinkPolicy, WriteAt,
};
const CLUSTER: u64 = 65536;

#[test]
fn active_resize_preserves_full_and_short_saved_states() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let mut writer = Qcow2Writer::create_sparse(&path, 3 * CLUSTER).unwrap();
    writer
        .write_all_at(0, &vec![37; (3 * CLUSTER) as usize])
        .unwrap();
    writer.create_snapshot(b"full", b"full").unwrap();
    assert_eq!(
        writer.inspection().capabilities.get(ImageOperation::Resize),
        Capability::Supported
    );
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        writer
            .resize(CLUSTER + 512, ShrinkPolicy::RequireZero)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    writer
        .write_zeroes(CLUSTER + 512, 2 * CLUSTER - 512)
        .unwrap();
    writer
        .resize(CLUSTER + 512, ShrinkPolicy::RequireZero)
        .unwrap();
    writer.create_snapshot(b"short", b"short").unwrap();
    writer.resize(3 * CLUSTER, ShrinkPolicy::Reject).unwrap();
    writer.write_all_at(7, b"private").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Arc::new(Qcow2::open_chain(&path, &[]).unwrap());
    disk.validate_active_mapping().unwrap();
    let mut bytes = vec![0; (3 * CLUSTER) as usize];
    disk.read_exact_at(0, &mut bytes).unwrap();
    let mut expected = vec![37; (CLUSTER + 512) as usize];
    expected.resize((3 * CLUSTER) as usize, 0);
    expected[7..14].copy_from_slice(b"private");
    assert_eq!(bytes, expected);
    let full = disk.open_snapshot(b"full").unwrap();
    assert_eq!(full.len(), 3 * CLUSTER);
    full.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|byte| *byte == 37));
    let short = disk.open_snapshot(b"short").unwrap();
    assert_eq!(short.len(), CLUSTER + 512);
    short
        .read_exact_at(0, &mut bytes[..(CLUSTER + 512) as usize])
        .unwrap();
    assert!(
        bytes[..(CLUSTER + 512) as usize]
            .iter()
            .all(|byte| *byte == 37)
    );
    drop((short, full, disk));
    let mut writer = Qcow2Writer::open(&path).unwrap();
    writer.revert_snapshot(b"full").unwrap();
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|byte| *byte == 37));
    writer.delete_snapshot(b"full").unwrap();
    writer.revert_snapshot(b"short").unwrap();
    assert_eq!(writer.len(), CLUSTER + 512);
    writer.delete_snapshot(b"short").unwrap();
}

#[test]
fn growth_from_empty_keeps_empty_snapshot_and_revert_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let mut writer = Qcow2Writer::create_sparse(&path, 0).unwrap();
    writer.create_snapshot(b"empty", b"empty").unwrap();
    writer.resize(CLUSTER + 512, ShrinkPolicy::Reject).unwrap();
    writer.write_all_at(0, b"private").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Arc::new(Qcow2::open_chain(&path, &[]).unwrap());
    disk.validate_active_mapping().unwrap();
    assert_eq!(disk.open_snapshot(b"empty").unwrap().len(), 0);
    drop(disk);
    let mut writer = Qcow2Writer::open(&path).unwrap();
    writer.revert_snapshot(b"empty").unwrap();
    assert_eq!(writer.len(), 0);
    writer.resize(CLUSTER, ShrinkPolicy::Reject).unwrap();
    let mut bytes = vec![37; CLUSTER as usize];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|byte| *byte == 0));
}

#[test]
#[ignore = "requires independent qemu-img snapshot resize ownership/readback oracle"]
fn qemu_reads_resized_active_and_original_saved_capacity() {
    use std::process::Command;
    for (old_size, new_size, policy) in [
        (CLUSTER + 512, 3 * CLUSTER, ShrinkPolicy::Reject),
        (3 * CLUSTER, CLUSTER + 512, ShrinkPolicy::AllowDataLoss),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image");
        let mut writer = Qcow2Writer::create_sparse(&path, old_size).unwrap();
        writer
            .write_all_at(0, &vec![37; old_size as usize])
            .unwrap();
        writer.create_snapshot(b"saved", b"saved").unwrap();
        writer.resize(new_size, policy).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let before = std::fs::read(&path).unwrap();
        let result = Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        for saved in [false, true] {
            let output = dir.path().join(if saved { "saved" } else { "active" });
            // QEMU's temporary snapshot loader switches mappings but keeps the
            // active capacity. Restore a copy to verify saved capacity as well.
            let source = if saved {
                let restored = dir.path().join("restored.qcow2");
                std::fs::copy(&path, &restored).unwrap();
                let result = Command::new("qemu-img")
                    .args(["snapshot", "-a", "saved"])
                    .arg(&restored)
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                restored
            } else {
                path.clone()
            };
            let result = Command::new("qemu-img")
                .args(["convert", "-f", "qcow2", "-O", "raw"])
                .arg(&source)
                .arg(&output)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let mut expected = vec![37; old_size as usize];
            if !saved {
                expected.resize(new_size as usize, 0);
            }
            let actual = std::fs::read(output).unwrap();
            assert!(
                actual == expected,
                "independent stream mismatch: saved={saved}, old={old_size}, new={new_size}, actual={}, expected={}, first_difference={:?}",
                actual.len(),
                expected.len(),
                actual.iter().zip(&expected).position(|(a, b)| a != b)
            );
        }
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[cfg(feature = "cli")]
#[test]
fn cli_native_resize_preserves_saved_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let mut writer = Qcow2Writer::create_sparse(&path, CLUSTER + 512).unwrap();
    writer
        .write_all_at(0, &vec![37; (CLUSTER + 512) as usize])
        .unwrap();
    writer.create_snapshot(b"saved", b"saved").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("resize-native")
        .arg(&path)
        .args(["qcow2", "196608", "reject"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let disk = Arc::new(Qcow2::open_chain(&path, &[]).unwrap());
    disk.validate_active_mapping().unwrap();
    assert_eq!(disk.len(), 3 * CLUSTER);
    let mut expected = vec![37; (CLUSTER + 512) as usize];
    expected.resize((3 * CLUSTER) as usize, 0);
    let mut bytes = vec![0; (3 * CLUSTER) as usize];
    disk.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(disk.open_snapshot(b"saved").unwrap().len(), CLUSTER + 512);
}

#[test]
fn backed_resize_masks_growth_and_checks_inherited_shrink_tail() {
    for format in [virtdisk::ImageFormat::Raw, virtdisk::ImageFormat::Qcow2] {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = dir.path().join("child");
        let writer = virtdisk::ImageWriter::create(&parent, format, 3 * CLUSTER).unwrap();
        writer
            .write_all_at(0, &vec![37; (3 * CLUSTER) as usize])
            .unwrap();
        writer.flush().unwrap();
        drop(writer);
        let parent_bytes = std::fs::read(&parent).unwrap();
        virtdisk::create_qcow2_overlay(
            &child,
            &parent,
            if format == virtdisk::ImageFormat::Raw {
                "raw"
            } else {
                "qcow2"
            },
            CLUSTER + 512,
        )
        .unwrap();
        let parents = std::slice::from_ref(&parent);
        let mut writer = Qcow2Writer::open_chain(&child, parents).unwrap();
        assert_eq!(
            writer.inspection().capabilities.get(ImageOperation::Resize),
            Capability::Supported
        );
        writer.create_snapshot(b"inherited", b"inherited").unwrap();
        let before = std::fs::read(&child).unwrap();
        assert_eq!(
            writer
                .resize(512, ShrinkPolicy::RequireZero)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(std::fs::read(&child).unwrap(), before);
        writer
            .resize(3 * CLUSTER + 512, ShrinkPolicy::Reject)
            .unwrap();
        let mut expected = vec![37; (CLUSTER + 512) as usize];
        expected.resize((3 * CLUSTER + 512) as usize, 0);
        let mut bytes = vec![0; expected.len()];
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
        writer
            .resize(CLUSTER + 512, ShrinkPolicy::RequireZero)
            .unwrap();
        writer
            .resize(3 * CLUSTER + 512, ShrinkPolicy::Reject)
            .unwrap();
        writer.resize(512, ShrinkPolicy::AllowDataLoss).unwrap();
        writer.resize(3 * CLUSTER, ShrinkPolicy::Reject).unwrap();
        expected = vec![37; 512];
        expected.resize((3 * CLUSTER) as usize, 0);
        bytes.resize(expected.len(), 0);
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
        writer.flush().unwrap();
        drop(writer);
        assert!(Qcow2Writer::open(&child).is_err());
        let disk = Arc::new(Qcow2::open_chain(&child, parents).unwrap());
        disk.validate_active_mapping().unwrap();
        let saved = disk.open_snapshot(b"inherited").unwrap();
        assert_eq!(saved.len(), CLUSTER + 512);
        bytes.resize(saved.len() as usize, 0);
        saved.read_exact_at(0, &mut bytes).unwrap();
        assert!(bytes.iter().all(|byte| *byte == 37));
        assert_eq!(std::fs::read(parent).unwrap(), parent_bytes);
    }
}

#[test]
#[ignore = "requires independent qemu-img backed resize ownership/readback oracle"]
fn qemu_reads_backed_resize_masks_and_saved_inheritance() {
    use std::process::Command;
    for format in [virtdisk::ImageFormat::Raw, virtdisk::ImageFormat::Qcow2] {
        for new_size in [512, 3 * CLUSTER + 512] {
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("parent");
            let child = dir.path().join("child");
            let old_size = CLUSTER + 512;
            let writer = virtdisk::ImageWriter::create(&parent, format, 3 * CLUSTER).unwrap();
            writer
                .write_all_at(0, &vec![37; (3 * CLUSTER) as usize])
                .unwrap();
            writer.flush().unwrap();
            drop(writer);
            virtdisk::create_qcow2_overlay(
                &child,
                &parent,
                if format == virtdisk::ImageFormat::Raw {
                    "raw"
                } else {
                    "qcow2"
                },
                old_size,
            )
            .unwrap();
            let mut writer =
                Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
            writer.create_snapshot(b"saved", b"saved").unwrap();
            writer
                .resize(new_size, ShrinkPolicy::AllowDataLoss)
                .unwrap();
            writer.flush().unwrap();
            drop(writer);
            let parent_bytes = std::fs::read(&parent).unwrap();
            let child_bytes = std::fs::read(&child).unwrap();
            for saved in [false, true] {
                let source = dir.path().join(if saved { "restored" } else { "active" });
                std::fs::copy(&child, &source).unwrap();
                if saved {
                    let result = Command::new("qemu-img")
                        .args(["snapshot", "-a", "saved"])
                        .arg(&source)
                        .output()
                        .unwrap();
                    assert!(
                        result.status.success(),
                        "{}",
                        String::from_utf8_lossy(&result.stderr)
                    );
                }
                let result = Command::new("qemu-img")
                    .args(["check", "-f", "qcow2"])
                    .arg(&source)
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                let output = dir
                    .path()
                    .join(if saved { "saved.raw" } else { "active.raw" });
                let result = Command::new("qemu-img")
                    .args(["convert", "-f", "qcow2", "-O", "raw"])
                    .arg(&source)
                    .arg(&output)
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                let mut expected = vec![37; old_size as usize];
                if !saved {
                    expected.resize(new_size as usize, 0);
                }
                assert_eq!(std::fs::read(output).unwrap(), expected);
            }
            assert_eq!(std::fs::read(parent).unwrap(), parent_bytes);
            assert_eq!(std::fs::read(child).unwrap(), child_bytes);
        }
    }
}
