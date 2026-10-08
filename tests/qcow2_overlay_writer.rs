#![cfg(feature = "std")]
#![cfg(target_os = "linux")]

use virtdisk::{Qcow2, Qcow2Writer, ReadAt, create_qcow2_overlay};

#[test]
#[cfg(target_os = "linux")]
fn authorized_overlay_partial_writes_copy_parent_and_preserve_immutable_base() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.raw");
    let child = dir.path().join("child.qcow2");
    let expected: Vec<u8> = (0..131072).map(|i| (i % 251) as u8).collect();
    std::fs::write(&parent, &expected).unwrap();
    create_qcow2_overlay(&child, &parent, "raw", 196608).unwrap();
    assert!(Qcow2Writer::open(&child).is_err());
    assert!(Qcow2Writer::open_chain(&child, &[]).is_err());
    let writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    let mut before = [0; 8];
    writer.read_exact_at(65532, &mut before).unwrap();
    assert!(before == expected[65532..65540]);
    writer.write_all_at(65534, &[7; 4]).unwrap();
    writer.write_zeroes(100, 17).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    disk.validate_active_mapping().unwrap();
    let mut actual = vec![0; 196608];
    disk.read_exact_at(0, &mut actual).unwrap();
    let mut model = expected.clone();
    model.resize(196608, 0);
    model[65534..65538].fill(7);
    model[100..117].fill(0);
    assert!(actual == model);
    assert!(std::fs::read(&parent).unwrap() == expected);
}

#[test]
#[cfg(target_os = "linux")]
fn qcow_parent_and_explicit_zero_mapping_preserve_authorization_and_masking() {
    use virtdisk::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.qcow2");
    let child = dir.path().join("child.qcow2");
    let writer = Qcow2Writer::create(&parent, 131072).unwrap();
    writer.write_all_at(0, &vec![9; 131072]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    create_qcow2_overlay(&child, &parent, "qcow2", 131072).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&child)
        .unwrap();
    file.set_len(5 * 65536).unwrap();
    file.seek(SeekFrom::Start(65536)).unwrap();
    file.write_all(&((4u64 * 65536) | (1 << 63)).to_be_bytes())
        .unwrap();
    file.seek(SeekFrom::Start(3 * 65536 + 8)).unwrap();
    file.write_all(&1u16.to_be_bytes()).unwrap();
    file.seek(SeekFrom::Start(4 * 65536)).unwrap();
    file.write_all(&1u64.to_be_bytes()).unwrap();
    drop(file);
    let writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(100, &[7; 4]).unwrap();
    let mut actual = [9; 8];
    writer.read_exact_at(96, &mut actual).unwrap();
    assert_eq!(actual, [0, 0, 0, 0, 7, 7, 7, 7]);
    writer.read_exact_at(65536, &mut actual).unwrap();
    assert_eq!(actual, [9; 8]);
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
#[cfg(target_os = "linux")]
fn qemu_checks_and_flattens_written_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.raw");
    let child = dir.path().join("child.qcow2");
    let raw = dir.path().join("flat.raw");
    let mut model = vec![9; 131072];
    std::fs::write(&parent, &model).unwrap();
    create_qcow2_overlay(&child, &parent, "raw", 131072).unwrap();
    let writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(65534, &[7; 4]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    model[65534..65538].fill(7);
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
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert!(std::fs::read(raw).unwrap() == model);
}

#[test]
#[cfg(target_os = "linux")]
fn every_parent_in_deeper_chain_must_be_authorized_and_cycles_fail() {
    use virtdisk::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.raw");
    let parent = dir.path().join("parent.qcow2");
    let child = dir.path().join("child.qcow2");
    std::fs::write(&base, vec![9; 131072]).unwrap();
    create_qcow2_overlay(&parent, &base, "raw", 131072).unwrap();
    // Build a native chain fixture: the backing-format extension has the same
    // eight-byte padded length for raw and qcow2.
    create_qcow2_overlay(&child, &parent, "raw", 131072).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&child)
        .unwrap();
    file.seek(SeekFrom::Start(108)).unwrap();
    file.write_all(&5u32.to_be_bytes()).unwrap();
    file.write_all(b"qcow2").unwrap();
    drop(file);
    assert!(Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).is_err());
    let writer = Qcow2Writer::open_chain(&child, &[parent.clone(), base.clone()]).unwrap();
    writer.write_all_at(19, &[7; 512]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Qcow2::open_chain(&child, &[parent.clone(), base]).unwrap();
    disk.validate_active_mapping().unwrap();
    let mut actual = [0; 8];
    disk.read_exact_at(65536, &mut actual).unwrap();
    assert_eq!(actual, [9; 8]);
    drop(disk);
    // Turn the parent's raw backing into the child itself; root path/identity
    // must be seeded even though the writer does not reopen its child handle.
    let name = child.canonicalize().unwrap();
    let name = name.to_str().unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&parent)
        .unwrap();
    file.seek(SeekFrom::Start(16)).unwrap();
    file.write_all(&(name.len() as u32).to_be_bytes()).unwrap();
    file.seek(SeekFrom::Start(128)).unwrap();
    file.write_all(name.as_bytes()).unwrap();
    drop(file);
    assert!(Qcow2Writer::open_chain(&child, &[parent, child.clone()]).is_err());
}
