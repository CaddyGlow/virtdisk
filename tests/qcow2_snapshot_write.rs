#![cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};
static SERIAL: Mutex<()> = Mutex::new(());
use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt};
#[test]
fn native_creation_preserves_saved_view_and_reopens_for_active_cow() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let mut writer = Qcow2Writer::create_sparse(&path, 131072).unwrap();
    writer.write_all_at(500, b"before").unwrap();
    let first = writer.create_snapshot(b"one", b"saved disk").unwrap();
    assert_eq!(first.id, b"one");
    writer.write_all_at(500, b"after!").unwrap();
    writer.write_zeroes(65536, 65536).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
    disk.validate_active_mapping().unwrap();
    let saved = disk.open_snapshot(b"one").unwrap();
    let mut bytes = [0; 6];
    saved.read_exact_at(500, &mut bytes).unwrap();
    assert_eq!(&bytes, b"before");
    disk.read_exact_at(500, &mut bytes).unwrap();
    assert_eq!(&bytes, b"after!");
    drop(saved);
    drop(disk);
    let mut writer = Qcow2Writer::open(&path).unwrap();
    writer.create_snapshot(b"two", b"second").unwrap();
    writer.write_all_at(500, b"latest").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
    assert_eq!(disk.list_snapshots().unwrap().len(), 2);
    disk.validate_active_mapping().unwrap();
    let saved = disk.open_snapshot(b"two").unwrap();
    saved.read_exact_at(500, &mut bytes).unwrap();
    assert_eq!(&bytes, b"after!");
}
#[test]
fn snapshot_invalid_requests_fail_before_metadata_mutation() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let mut writer = Qcow2Writer::create_sparse(&path, 65536).unwrap();
    writer.create_snapshot(b"id", b"name").unwrap();
    writer.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    for (id, name) in [
        (b"".as_slice(), b"name".as_slice()),
        (b"id".as_slice(), b"duplicate".as_slice()),
        (b"other".as_slice(), &[1; 257]),
    ] {
        assert!(writer.create_snapshot(id, name).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn near_full_native_directory_and_resize_are_rejected_before_mutation() {
    let _guard = SERIAL.lock().unwrap();
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("near-full.qcow2");
    let mut writer = Qcow2Writer::create_sparse(&path, 65536).unwrap();
    writer.create_snapshot(b"id", b"name").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let bytes = std::fs::read(&path).unwrap();
    let directory = u64::from_be_bytes(bytes[64..72].try_into().unwrap());
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(directory + 14)).unwrap();
    file.write_all(&65470u16.to_be_bytes()).unwrap();
    drop(file);
    let mut writer = Qcow2Writer::open(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        writer.create_snapshot(b"x", b"y").unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        writer
            .resize(131072, virtdisk::ShrinkPolicy::RequireZero)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
#[ignore = "requires independent qemu-img native snapshot creation/readback oracle"]
fn qemu_checks_lists_and_extracts_native_saved_state_after_cow() {
    let _guard = SERIAL.lock().unwrap();
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let mut writer = Qcow2Writer::create_sparse(&path, 131072).unwrap();
    writer.write_all_at(500, b"before").unwrap();
    writer
        .create_snapshot(b"native-one", b"native saved")
        .unwrap();
    writer.write_all_at(500, b"after!").unwrap();
    writer.flush().unwrap();
    drop(writer);
    for args in [vec!["check"], vec!["snapshot", "-l"]] {
        let out = Command::new("qemu-img")
            .args(args)
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let saved = dir.path().join("saved.raw");
    let out = Command::new("qemu-img")
        .args([
            "convert",
            "-f",
            "qcow2",
            "-O",
            "raw",
            "-l",
            "snapshot.id=native-one",
        ])
        .arg(&path)
        .arg(&saved)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(&std::fs::read(saved).unwrap()[500..506], b"before");
    let active = dir.path().join("active.raw");
    let out = Command::new("qemu-img")
        .args(["convert", "-f", "qcow2", "-O", "raw"])
        .arg(&path)
        .arg(&active)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(&std::fs::read(active).unwrap()[500..506], b"after!");
    // A native QEMU snapshot can subsequently coexist with our saved state.
    let out = Command::new("qemu-img")
        .args(["snapshot", "-c", "qemu-next"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut writer = Qcow2Writer::open(&path).unwrap();
    writer
        .create_snapshot(b"native-three", b"after qemu")
        .unwrap();
    writer.write_zeroes(500, 6).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let out = Command::new("qemu-img")
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn oversized_metadata_transaction_is_rejected_without_publication() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wide.qcow2");
    let mut writer = Qcow2Writer::create_sparse(&path, 6 * 1024 * 1024 * 1024).unwrap();
    for index in 0..12 {
        writer
            .write_all_at(index * 512 * 1024 * 1024, &[7])
            .unwrap();
    }
    writer.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        writer
            .create_snapshot(b"too-wide", b"bounded")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    writer.write_all_at(0, &[8]).unwrap();
    writer.flush().unwrap();
}

#[test]
fn shared_active_and_saved_l1_is_readable_but_writer_refuses_before_mutation() {
    let _guard = SERIAL.lock().unwrap();
    for multi_cluster in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared-l1.qcow2");
        let mut writer = Qcow2Writer::create_sparse(&path, 65536).unwrap();
        writer.write_all_at(500, b"saved!").unwrap();
        writer
            .create_snapshot(b"saved", b"shared L1 regression")
            .unwrap();
        writer.flush().unwrap();
        drop(writer);
        let mut bytes = std::fs::read(&path).unwrap();
        let get = |at| u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap());
        let active = get(40);
        let directory = get(64);
        let saved = get(directory as usize);
        let ref_table = get(48);
        let counter = |offset: u64| {
            let cluster = offset / 65536;
            let block = u64::from_be_bytes(
                bytes[ref_table as usize + (cluster / 32768) as usize * 8
                    ..ref_table as usize + (cluster / 32768) as usize * 8 + 8]
                    .try_into()
                    .unwrap(),
            );
            block as usize + (cluster % 32768) as usize * 2
        };
        let active_counter = counter(active);
        let saved_counter = counter(saved);
        if multi_cluster {
            let relocated = bytes.len() as u64;
            let first_counter = counter(relocated);
            let tail_counter = counter(relocated + 65536);
            let old_l1 = bytes[active as usize..active as usize + 65536].to_vec();
            bytes.resize(relocated as usize + 131072, 0);
            bytes[relocated as usize..relocated as usize + 65536].copy_from_slice(&old_l1);
            bytes[40..48].copy_from_slice(&relocated.to_be_bytes());
            bytes[36..40].copy_from_slice(&8193u32.to_be_bytes());
            bytes[directory as usize..directory as usize + 8]
                .copy_from_slice(&relocated.to_be_bytes());
            bytes[directory as usize + 8..directory as usize + 12]
                .copy_from_slice(&8193u32.to_be_bytes());
            bytes[active_counter..active_counter + 2].copy_from_slice(&0u16.to_be_bytes());
            bytes[first_counter..first_counter + 2].copy_from_slice(&2u16.to_be_bytes());
            bytes[tail_counter..tail_counter + 2].copy_from_slice(&2u16.to_be_bytes());
        } else {
            bytes[directory as usize..directory as usize + 8]
                .copy_from_slice(&active.to_be_bytes());
            bytes[active_counter..active_counter + 2].copy_from_slice(&2u16.to_be_bytes());
        }
        bytes[saved_counter..saved_counter + 2].copy_from_slice(&0u16.to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let reader = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
        reader.validate_active_mapping().unwrap();
        let view = reader.open_snapshot(b"saved").unwrap();
        let mut out = [0; 6];
        view.read_exact_at(500, &mut out).unwrap();
        assert_eq!(&out, b"saved!");
        drop(view);
        drop(reader);
        match Qcow2Writer::open(&path) {
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::Unsupported),
            Ok(writer) => {
                let error = writer.write_all_at(500, b"later!").unwrap_err();
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                panic!("shared active L1 was accepted; COW rejected without mutation: {error}");
            }
        };
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
