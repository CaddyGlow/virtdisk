#![cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};
use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt};
static SERIAL: Mutex<()> = Mutex::new(());
fn read_state(path: &std::path::Path, id: Option<&[u8]>) -> Vec<u8> {
    let disk = Arc::new(Qcow2::open(Arc::new(RawDisk::open(path).unwrap())).unwrap());
    disk.validate_active_mapping().unwrap();
    let source: Arc<dyn ReadAt> = if let Some(id) = id {
        Arc::new(disk.open_snapshot(id).unwrap())
    } else {
        disk
    };
    let mut bytes = vec![0; source.len() as usize];
    source.read_exact_at(0, &mut bytes).unwrap();
    bytes
}
#[test]
fn delete_middle_first_and_last_preserves_active_and_surviving_states() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("delete.qcow2");
    let mut w = Qcow2Writer::create_sparse(&path, 131072).unwrap();
    for (id, value) in [(b"a", 11), (b"b", 22), (b"c", 33)] {
        w.write_all_at(500, &[value; 9]).unwrap();
        w.create_snapshot(id, b"state").unwrap();
    }
    w.write_all_at(500, &[44; 9]).unwrap();
    assert_eq!(w.delete_snapshot(b"b").unwrap().id, b"b");
    w.flush().unwrap();
    drop(w);
    assert_eq!(&read_state(&path, None)[500..509], &[44; 9]);
    assert_eq!(&read_state(&path, Some(b"a"))[500..509], &[11; 9]);
    assert_eq!(&read_state(&path, Some(b"c"))[500..509], &[33; 9]);
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.delete_snapshot(b"a").unwrap();
    w.delete_snapshot(b"c").unwrap();
    w.write_all_at(501, &[55]).unwrap();
    w.flush().unwrap();
    drop(w);
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    assert!(disk.list_snapshots().unwrap().is_empty());
    disk.validate_active_mapping().unwrap();
    let header = std::fs::read(path).unwrap();
    assert_eq!(&header[60..72], &[0; 12]);
}
#[test]
fn revert_retains_snapshots_and_partial_followup_write_cows_saved_bytes() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("revert.qcow2");
    let mut w = Qcow2Writer::create_sparse(&path, 131072).unwrap();
    w.write_all_at(500, &[17; 12]).unwrap();
    w.create_snapshot(b"a", b"first").unwrap();
    w.write_all_at(500, &[29; 12]).unwrap();
    w.create_snapshot(b"b", b"second").unwrap();
    w.write_zeroes(0, 65536).unwrap();
    let selected = w.revert_snapshot(b"a").unwrap();
    assert_eq!(selected.id, b"a");
    assert_eq!(w.len(), 131072);
    let mut bytes = [0; 12];
    w.read_exact_at(500, &mut bytes).unwrap();
    assert_eq!(bytes, [17; 12]);
    w.write_all_at(505, &[61; 2]).unwrap();
    w.flush().unwrap();
    drop(w);
    assert_eq!(&read_state(&path, Some(b"a"))[500..512], &[17; 12]);
    assert_eq!(&read_state(&path, Some(b"b"))[500..512], &[29; 12]);
    let active = read_state(&path, None);
    assert_eq!(&active[505..507], &[61; 2]);
}
#[test]
fn missing_ids_leave_container_unchanged_and_writer_usable() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.qcow2");
    let mut w = Qcow2Writer::create_sparse(&path, 65536).unwrap();
    w.create_snapshot(b"exists", b"name").unwrap();
    w.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        w.delete_snapshot(b"absent").unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(
        w.revert_snapshot(b"absent").unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    w.write_all_at(0, &[5]).unwrap();
}

#[test]
fn deletion_preserves_unknown_snapshot_extra_bytes_and_binary_identifiers() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extras.qcow2");
    let mut w = Qcow2Writer::create_sparse(&path, 65536).unwrap();
    w.create_snapshot(&[0, 255], &[128, 0, 9]).unwrap();
    w.create_snapshot(b"delete", b"name").unwrap();
    w.flush().unwrap();
    drop(w);
    let mut bytes = std::fs::read(&path).unwrap();
    let directory = u64::from_be_bytes(bytes[64..72].try_into().unwrap()) as usize;
    // Add 16 bounded opaque extra bytes to the retained first native record.
    bytes.copy_within(directory + 56..directory + 136, directory + 72);
    bytes[directory + 36..directory + 40].copy_from_slice(&32u32.to_be_bytes());
    bytes[directory + 56..directory + 72].copy_from_slice(&[0x93; 16]);
    let retained = bytes[directory..directory + 80].to_vec();
    std::fs::write(&path, &bytes).unwrap();
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.delete_snapshot(b"delete").unwrap();
    w.flush().unwrap();
    drop(w);
    let bytes = std::fs::read(&path).unwrap();
    let new_directory = u64::from_be_bytes(bytes[64..72].try_into().unwrap()) as usize;
    assert_eq!(&bytes[new_directory..new_directory + 80], &retained);
    let disk = Qcow2::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    assert_eq!(disk.list_snapshots().unwrap()[0].id, [0, 255]);
}

#[test]
#[ignore = "requires independent qemu-img/qemu-io native lifecycle and capacity oracle"]
fn native_qemu_saved_capacities_delete_revert_and_followup_writes() {
    use std::{path::Path, process::Command};
    let _guard = SERIAL.lock().unwrap();
    fn run(program: &str, args: &[&str], path: &Path) {
        let out = Command::new(program).args(args).arg(path).output().unwrap();
        assert!(
            out.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn export(path: &Path, output: &Path, id: Option<&str>) -> Vec<u8> {
        let mut command = Command::new("qemu-img");
        command.args(["convert", "-f", "qcow2", "-O", "raw"]);
        if let Some(id) = id {
            command.args(["-l", &format!("snapshot.name={id}")]);
        }
        let out = command.arg(path).arg(output).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read(output).unwrap()
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.qcow2");
    let mut empty = Qcow2Writer::create_sparse(&path, 0).unwrap();
    empty.create_snapshot(b"empty", b"empty").unwrap();
    empty.flush().unwrap();
    drop(empty);
    resize(&path, "128K");
    run(
        "qemu-io",
        &["-f", "qcow2", "-c", "write -P 0x17 0 131072"],
        &path,
    );
    run("qemu-img", &["snapshot", "-c", "large"], &path);
    fn resize(path: &Path, size: &str) {
        let out = Command::new("qemu-img")
            .args(["resize", "--shrink"])
            .arg(path)
            .arg(size)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    resize(&path, "64K");
    run(
        "qemu-io",
        &["-f", "qcow2", "-c", "write -P 0x29 0 65536"],
        &path,
    );
    run("qemu-img", &["snapshot", "-c", "small"], &path);
    resize(&path, "128K");
    let states = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap()))
        .unwrap()
        .list_snapshots()
        .unwrap();
    let id = |name: &[u8]| states.iter().find(|s| s.name == name).unwrap().id.clone();
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.revert_snapshot(&id(b"large")).unwrap();
    assert_eq!(w.len(), 131072);
    let mut out = [0; 4];
    w.read_exact_at(500, &mut out).unwrap();
    assert_eq!(out, [0x17; 4]);
    w.write_all_at(500, &[0x71]).unwrap();
    w.revert_snapshot(&id(b"small")).unwrap();
    assert_eq!(w.len(), 65536);
    w.read_exact_at(500, &mut out).unwrap();
    assert_eq!(out, [0x29; 4]);
    w.flush().unwrap();
    drop(w);
    assert_eq!(
        export(&path, &dir.path().join("small-active.raw"), None),
        vec![0x29; 65536]
    );
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.revert_snapshot(&id(b"empty")).unwrap();
    assert_eq!(w.len(), 0);
    assert!(w.read_exact_at(0, &mut out).is_err());
    w.flush().unwrap();
    drop(w);
    run("qemu-img", &["check"], &path);
    assert!(export(&path, &dir.path().join("empty-active.raw"), None).is_empty());
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.revert_snapshot(&id(b"large")).unwrap();
    w.write_all_at(500, &[0x71]).unwrap();
    w.delete_snapshot(&id(b"small")).unwrap();
    w.flush().unwrap();
    drop(w);
    run("qemu-img", &["check"], &path);
    run("qemu-img", &["snapshot", "-l"], &path);
    assert_eq!(
        export(&path, &dir.path().join("large.raw"), Some("large")),
        vec![0x17; 131072]
    );
    // QEMU's read-only snapshot_load_tmp replaces L1, not current virtual size.
    assert_eq!(
        export(&path, &dir.path().join("empty.raw"), Some("empty")),
        vec![0; 131072]
    );
    let mut expected = vec![0x17; 131072];
    expected[500] = 0x71;
    assert_eq!(
        export(&path, &dir.path().join("active.raw"), None),
        expected
    );
    run("qemu-img", &["snapshot", "-a", "large"], &path);
    run("qemu-img", &["snapshot", "-d", "empty"], &path);
    run("qemu-img", &["check"], &path);
    assert_eq!(read_state(&path, None), vec![0x17; 131072]);
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.create_snapshot(b"after-native", b"later").unwrap();
    w.delete_snapshot(&id(b"large")).unwrap();
    w.flush().unwrap();
    drop(w);
    run("qemu-img", &["check"], &path);
}

#[test]
fn repeated_l2_payload_owners_and_preallocated_zero_survive_lifecycle_cow() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aliases.qcow2");
    let w = Qcow2Writer::create(&path, 65536).unwrap();
    w.write_all_at(0, &[7; 16]).unwrap();
    w.flush().unwrap();
    drop(w);
    let mut bytes = std::fs::read(&path).unwrap();
    let get = |at| u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap());
    let l1 = get(40) as usize;
    let l2 = (get(l1) & 0x00ff_ffff_ffff_fe00) as usize;
    let payload = get(l2) & 0x00ff_ffff_ffff_fe00;
    let table = get(48) as usize;
    let block = get(table) as usize;
    bytes[24..32].copy_from_slice(&(512u64 * 1024 * 1024 + 65536).to_be_bytes());
    bytes[36..40].copy_from_slice(&2u32.to_be_bytes());
    bytes[l1..l1 + 8].copy_from_slice(&(l2 as u64).to_be_bytes());
    bytes[l1 + 8..l1 + 16].copy_from_slice(&(l2 as u64).to_be_bytes());
    bytes[l2..l2 + 8].copy_from_slice(&(payload | 1).to_be_bytes());
    for offset in [l2 as u64, payload] {
        let at = block + (offset / 65536) as usize * 2;
        bytes[at..at + 2].copy_from_slice(&2u16.to_be_bytes());
    }
    std::fs::write(&path, bytes).unwrap();
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.create_snapshot(b"zero", b"alias").unwrap();
    w.write_all_at(1, &[8]).unwrap();
    w.revert_snapshot(b"zero").unwrap();
    let boundary = 512 * 1024 * 1024;
    w.write_all_at(boundary + 500, &[9]).unwrap();
    w.delete_snapshot(b"zero").unwrap();
    w.flush().unwrap();
    let mut zeros = [255; 16];
    w.read_exact_at(0, &mut zeros).unwrap();
    assert_eq!(zeros, [0; 16]);
    let mut byte = [0];
    w.read_exact_at(boundary + 500, &mut byte).unwrap();
    assert_eq!(byte, [9]);
    w.write_all_at(0, &[11]).unwrap();
    w.flush().unwrap();
    drop(w);
    let disk = Qcow2::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    disk.read_exact_at(0, &mut byte).unwrap();
    assert_eq!(byte, [11]);
    disk.read_exact_at(boundary + 500, &mut byte).unwrap();
    assert_eq!(byte, [9]);
}

#[test]
#[ignore = "requires qemu-img/qemu-io compressed saved-state fixture"]
fn compressed_selected_state_reverts_and_deletes_without_changing_saved_bytes() {
    use std::process::Command;
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("compressed.raw");
    let path = dir.path().join("compressed.qcow2");
    let mut expected = vec![0x55; 196608];
    let mut noise = 0x12345678u32;
    for byte in &mut expected[131072..] {
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        *byte = noise as u8;
    }
    std::fs::write(&raw, &expected).unwrap();
    let out = Command::new("qemu-img")
        .args(["convert", "-f", "raw", "-O", "qcow2", "-c"])
        .arg(raw)
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success());
    for (program, args) in [
        ("qemu-img", vec!["snapshot", "-c", "compressed"]),
        ("qemu-img", vec!["snapshot", "-c", "compressed-sibling"]),
        (
            "qemu-io",
            vec!["-f", "qcow2", "-c", "write -P 0x44 0 196608"],
        ),
    ] {
        let out = Command::new(program)
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
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let audit = disk
        .validate_active_mapping_and_compressed_payloads()
        .unwrap();
    assert_eq!(audit.compressed_clusters, 4);
    assert_eq!(audit.data_descriptors, 9);
    let snapshots = disk.list_snapshots().unwrap();
    let id = snapshots[0].id.clone();
    let sibling = snapshots[1].id.clone();
    drop(disk);
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.revert_snapshot(&id).unwrap();
    let mut bytes = vec![0; 196608];
    w.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, expected);
    w.create_snapshot(b"plain", b"uncompressed").unwrap();
    w.delete_snapshot(b"plain").unwrap();
    w.write_all_at(5, &[0x77]).unwrap();
    w.flush().unwrap();
    drop(w);
    assert_eq!(read_state(&path, Some(&id)), expected);
    let saved_raw = dir.path().join("saved.raw");
    let out = Command::new("qemu-img")
        .args([
            "convert",
            "-l",
            &format!("snapshot.id={}", String::from_utf8_lossy(&id)),
            "-O",
            "raw",
        ])
        .arg(&path)
        .arg(&saved_raw)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(saved_raw).unwrap(), expected);
    let mut w = Qcow2Writer::open(&path).unwrap();
    w.delete_snapshot(&id).unwrap();
    w.flush().unwrap();
    drop(w);
    assert_eq!(read_state(&path, Some(&sibling)), expected);
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
fn already_empty_revert_succeeds_without_publishing_empty_transaction() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.qcow2");
    let mut w = Qcow2Writer::create_sparse(&path, 0).unwrap();
    w.create_snapshot(b"empty", b"empty").unwrap();
    // The exporter may retain an ignored L1 pointer for zero entries. First
    // revert canonicalizes it; repeating the same revert has no changed fields.
    w.revert_snapshot(b"empty").unwrap();
    w.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    let selected = w.revert_snapshot(b"empty").unwrap();
    assert_eq!(selected.virtual_size, 0);
    assert_eq!(w.len(), 0);
    assert!(
        std::fs::read(&path).unwrap() == before,
        "empty no-op changed container"
    );
    w.delete_snapshot(b"empty").unwrap();
    w.flush().unwrap();
    drop(w);
    let disk = Qcow2::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    disk.validate_active_mapping().unwrap();
    assert!(disk.list_snapshots().unwrap().is_empty());
}

#[test]
#[ignore = "requires qemu-img/qemu-io compressed transaction budget fixture"]
fn compressed_materialization_budget_refuses_before_mutation() {
    use std::process::Command;
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("large.raw");
    let path = dir.path().join("large.qcow2");
    std::fs::write(&raw, vec![0x55; 16 * 65536]).unwrap();
    for (program, args) in [
        (
            "qemu-img",
            vec!["convert", "-f", "raw", "-O", "qcow2", "-c"],
        ),
        ("qemu-img", vec!["snapshot", "-c", "compressed"]),
        (
            "qemu-io",
            vec!["-f", "qcow2", "-c", "write -P 0x44 0 1048576"],
        ),
    ] {
        let mut command = Command::new(program);
        command.args(args);
        if program == "qemu-img" && !path.exists() {
            command.arg(&raw);
        }
        let out = command.arg(&path).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let disk = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let id = disk.list_snapshots().unwrap()[0].id.clone();
    drop(disk);
    let mut writer = Qcow2Writer::open(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        writer.revert_snapshot(&id).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        !std::path::PathBuf::from(format!("{}.virtdisk-qcow2-journal", path.display())).exists()
    );
    writer.delete_snapshot(&id).unwrap();
    writer.write_all_at(5, &[0x77]).unwrap();
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
