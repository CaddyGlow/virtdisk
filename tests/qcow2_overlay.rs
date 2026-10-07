use virtdisk::{Qcow2, ReadAt, create_qcow2_overlay};

#[test]
fn raw_overlay_requires_authorization_and_inherits_parent() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.raw");
    let child = dir.path().join("child.qcow2");
    let data: Vec<u8> = (0..131072).map(|i| (i % 251) as u8).collect();
    std::fs::write(&parent, &data).unwrap();
    create_qcow2_overlay(&child, &parent, "raw", 196608).unwrap();
    assert!(Qcow2::open_chain(&child, &[]).is_err());
    let disk = Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    disk.validate_active_mapping().unwrap();
    let mut actual = vec![0; 196608];
    disk.read_exact_at(0, &mut actual).unwrap();
    assert!(actual[..131072] == data);
    assert!(actual[131072..].iter().all(|byte| *byte == 0));
    assert!(std::fs::read(parent).unwrap() == data);
}

#[test]
fn qcow_parent_inherits_and_creation_rejects_invalid_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.qcow2");
    let child = dir.path().join("child.qcow2");
    let writer = virtdisk::Qcow2Writer::create(&parent, 65536).unwrap();
    writer.write_all_at(0, &[7; 512]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    create_qcow2_overlay(&child, &parent, "qcow2", 65536).unwrap();
    let disk = Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    disk.validate_active_mapping().unwrap();
    let mut actual = [0; 512];
    disk.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, [7; 512]);
    let before = std::fs::read(&child).unwrap();
    assert!(create_qcow2_overlay(&child, &parent, "qcow2", 65536).is_err());
    assert!(std::fs::read(child).unwrap() == before);
    let absent = dir.path().join("absent");
    assert!(create_qcow2_overlay(&absent, &parent, "vmdk", 65536).is_err());
    assert!(create_qcow2_overlay(&absent, &parent, "raw", 513).is_err());
    assert!(create_qcow2_overlay(&absent, "https://example.invalid/disk", "raw", 512).is_err());
    assert!(!absent.exists());
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_checks_and_flattens_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.raw");
    let child = dir.path().join("child.qcow2");
    let flattened = dir.path().join("flattened.raw");
    let expected = vec![9; 131072];
    std::fs::write(&parent, &expected).unwrap();
    create_qcow2_overlay(&child, &parent, "raw", 131072).unwrap();
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
            .arg(&flattened)
            .status()
            .unwrap()
            .success()
    );
    assert!(std::fs::read(flattened).unwrap() == expected);
}

#[test]
fn explicit_zero_mapping_masks_parent_while_unallocated_inherits() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.raw");
    let child = dir.path().join("child.qcow2");
    std::fs::write(&parent, vec![7; 131072]).unwrap();
    create_qcow2_overlay(&child, &parent, "raw", 131072).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&child)
        .unwrap();
    file.set_len(5 * 65536).unwrap();
    file.seek(SeekFrom::Start(65536)).unwrap();
    file.write_all(&((4u64 * 65536) | (1 << 63)).to_be_bytes())
        .unwrap();
    file.seek(SeekFrom::Start(3 * 65536 + 4 * 2)).unwrap();
    file.write_all(&1u16.to_be_bytes()).unwrap();
    file.seek(SeekFrom::Start(4 * 65536)).unwrap();
    file.write_all(&1u64.to_be_bytes()).unwrap();
    drop(file);
    let disk = Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    disk.validate_active_mapping().unwrap();
    let mut actual = [9; 8];
    disk.read_exact_at(65532, &mut actual).unwrap();
    assert_eq!(actual, [0, 0, 0, 0, 7, 7, 7, 7]);
}
