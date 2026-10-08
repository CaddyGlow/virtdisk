#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use virtdisk::{ImageFormat, InspectImage, Qcow2Writer, ReadAt, WriteAt};

#[test]
fn authorized_backed_snapshot_lifecycle_preserves_inherited_private_and_zero_bytes() {
    for format in [ImageFormat::Raw, ImageFormat::Qcow2] {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = dir.path().join("child");
        let writer = virtdisk::ImageWriter::create(&parent, format, 131072).unwrap();
        writer.write_all_at(0, &vec![37; 131072]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let parent_bytes = std::fs::read(&parent).unwrap();
        virtdisk::create_qcow2_overlay(
            &child,
            &parent,
            if format == ImageFormat::Raw {
                "raw"
            } else {
                "qcow2"
            },
            131072,
        )
        .unwrap();
        let mut writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
        for operation in [
            virtdisk::ImageOperation::NativeSnapshotCreate,
            virtdisk::ImageOperation::NativeSnapshotDelete,
            virtdisk::ImageOperation::NativeSnapshotRevert,
        ] {
            assert_eq!(
                writer.inspection().capabilities.get(operation),
                virtdisk::Capability::Supported
            );
        }
        writer
            .create_snapshot(b"inherited", b"inherited parent")
            .unwrap();
        writer.write_all_at(7, b"private").unwrap();
        writer.discard(65536, 65536).unwrap();
        writer.flush().unwrap();
        let mut private = vec![37; 131072];
        private[7..14].copy_from_slice(b"private");
        private[65536..].fill(0);
        writer
            .create_snapshot(b"private", b"private and zero")
            .unwrap();
        writer.write_all_at(8, b"new").unwrap();
        writer.flush().unwrap();
        drop(writer);
        {
            let disk = std::sync::Arc::new(
                virtdisk::Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap(),
            );
            disk.validate_active_mapping().unwrap();
            let mut saved = vec![0; 131072];
            disk.open_snapshot(b"inherited")
                .unwrap()
                .read_exact_at(0, &mut saved)
                .unwrap();
            assert!(saved.iter().all(|byte| *byte == 37));
            disk.open_snapshot(b"private")
                .unwrap()
                .read_exact_at(0, &mut saved)
                .unwrap();
            assert_eq!(saved, private);
        }
        let mut writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
        writer.revert_snapshot(b"inherited").unwrap();
        let mut bytes = vec![0; 131072];
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert!(bytes.iter().all(|byte| *byte == 37));
        writer.revert_snapshot(b"private").unwrap();
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, private);
        writer.delete_snapshot(b"inherited").unwrap();
        writer.delete_snapshot(b"private").unwrap();
        drop(writer);
        let image = virtdisk::Image::open_chain(
            &child,
            Some(ImageFormat::Qcow2),
            std::slice::from_ref(&parent),
        )
        .unwrap();
        image.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, private);
        assert_eq!(std::fs::read(parent).unwrap(), parent_bytes);
    }
}

#[test]
fn backed_snapshot_writer_requires_parent_authority_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    let child = dir.path().join("child");
    std::fs::write(&parent, [37; 512]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &parent, "raw", 512).unwrap();
    let original = std::fs::read(&child).unwrap();
    assert!(Qcow2Writer::open(&child).is_err());
    assert_eq!(std::fs::read(&child).unwrap(), original);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[cfg(feature = "cli")]
#[test]
fn cli_backed_native_snapshot_lifecycle_requires_explicit_parent() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    let child = dir.path().join("child");
    std::fs::write(&parent, [37; 512]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &parent, "raw", 512).unwrap();
    let before = std::fs::read(&child).unwrap();
    let denied = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["snapshot", "create"])
        .arg(&child)
        .args(["saved", "name"])
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(2));
    assert_eq!(std::fs::read(&child).unwrap(), before);
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["snapshot", "create"])
        .arg(&child)
        .args(["saved", "name"])
        .arg(&parent)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(7, b"private").unwrap();
    writer.flush().unwrap();
    drop(writer);
    for action in ["revert", "delete"] {
        let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["snapshot", action])
            .arg(&child)
            .arg("saved")
            .arg(&parent)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let reader = virtdisk::Image::open_chain(
        &child,
        Some(ImageFormat::Qcow2),
        std::slice::from_ref(&parent),
    )
    .unwrap();
    let mut bytes = [0; 512];
    reader.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 512]);
    assert_eq!(std::fs::read(parent).unwrap(), [37; 512]);
}

#[test]
#[ignore = "requires independent qemu-img backed snapshot lifecycle oracle"]
fn qemu_reads_backed_active_and_saved_states_before_and_after_lifecycle() {
    use std::process::Command;
    for format in [ImageFormat::Raw, ImageFormat::Qcow2] {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = dir.path().join("child");
        let writer = virtdisk::ImageWriter::create(&parent, format, 131072).unwrap();
        writer.write_all_at(0, &vec![37; 131072]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let parent_bytes = std::fs::read(&parent).unwrap();
        virtdisk::create_qcow2_overlay(
            &child,
            &parent,
            if format == ImageFormat::Raw {
                "raw"
            } else {
                "qcow2"
            },
            131072,
        )
        .unwrap();
        let mut writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
        writer.create_snapshot(b"inherited", b"inherited").unwrap();
        writer.write_all_at(7, b"private").unwrap();
        writer.discard(65536, 65536).unwrap();
        writer.flush().unwrap();
        writer.create_snapshot(b"private", b"private").unwrap();
        let mut private = vec![37; 131072];
        private[7..14].copy_from_slice(b"private");
        private[65536..].fill(0);
        writer.write_all_at(8, b"new").unwrap();
        writer.flush().unwrap();
        drop(writer);
        let before_oracle = std::fs::read(&child).unwrap();
        let mut active = private.clone();
        active[8..11].copy_from_slice(b"new");
        let result = Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&child)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        for (id, expected) in [
            (None, active),
            (Some("inherited"), vec![37; 131072]),
            (Some("private"), private.clone()),
        ] {
            let output = dir.path().join(id.unwrap_or("active"));
            let mut command = Command::new("qemu-img");
            command.args(["convert", "-f", "qcow2", "-O", "raw"]);
            if let Some(id) = id {
                command.args(["-l", id]);
            }
            let result = command.arg(&child).arg(&output).output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(
                std::fs::read(output).unwrap() == expected,
                "independent saved/active stream mismatch"
            );
        }
        assert_eq!(std::fs::read(&child).unwrap(), before_oracle);
        let mut writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
        writer.revert_snapshot(b"private").unwrap();
        writer.delete_snapshot(b"inherited").unwrap();
        writer.delete_snapshot(b"private").unwrap();
        drop(writer);
        let result = Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&child)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let output = dir.path().join("final");
        let result = Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&child)
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(std::fs::read(output).unwrap() == private);
        assert_eq!(std::fs::read(parent).unwrap(), parent_bytes);
    }
}
