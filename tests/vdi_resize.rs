#![cfg(feature = "std")]
use virtdisk::{ShrinkPolicy, VdiWriter};
const M: u64 = 1 << 20;
#[test]
fn grow_shrink_and_regrow_preserve_only_retained_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let mut disk = VdiWriter::create_sparse(&path, M).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.resize(3 * M, ShrinkPolicy::Reject).unwrap();
    disk.write_all_at(2 * M, &[8]).unwrap();
    assert!(disk.resize(512, ShrinkPolicy::RequireZero).is_err());
    disk.resize(512, ShrinkPolicy::AllowDataLoss).unwrap();
    assert_eq!(disk.len(), 512);
    disk.resize(3 * M, ShrinkPolicy::Reject).unwrap();
    let mut bytes = [9; 1];
    disk.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [7]);
    disk.read_exact_at(2 * M, &mut bytes).unwrap();
    assert_eq!(bytes, [0]);
    disk.flush().unwrap();
    drop(disk);
    assert_eq!(VdiWriter::open(&path).unwrap().len(), 3 * M);
}
#[test]
fn empty_image_can_expand_its_native_map_arena() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let mut disk = VdiWriter::create_sparse(&path, 512).unwrap();
    disk.resize(129 * M, ShrinkPolicy::Reject).unwrap();
    disk.write_all_at(128 * M, &[4]).unwrap();
    disk.flush().unwrap();
    drop(disk);
    let disk = VdiWriter::open(&path).unwrap();
    let mut bytes = [0];
    disk.read_exact_at(128 * M, &mut bytes).unwrap();
    assert_eq!(bytes, [4]);
}
#[test]
fn invalid_shrink_does_not_mutate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let mut disk = VdiWriter::create_sparse(&path, 2 * M).unwrap();
    disk.write_all_at(0, &[3]).unwrap();
    disk.write_all_at(M, &[4]).unwrap();
    disk.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(disk.resize(512, ShrinkPolicy::Reject).is_err());
    assert!(disk.resize(0, ShrinkPolicy::AllowDataLoss).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn growth_never_exposes_hidden_partial_block_padding() {
    use virtdisk::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let disk = VdiWriter::create_sparse(&path, 512).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.flush().unwrap();
    drop(disk);
    let bytes = std::fs::read(&path).unwrap();
    let data = u32::from_le_bytes(bytes[344..348].try_into().unwrap()) as u64;
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(data + 512)).unwrap();
    file.write_all(&vec![9; M as usize - 512]).unwrap();
    drop(file);
    let mut disk = VdiWriter::open(&path).unwrap();
    disk.resize(M, ShrinkPolicy::Reject).unwrap();
    let mut out = vec![1; M as usize - 512];
    disk.read_exact_at(512, &mut out).unwrap();
    assert!(out.iter().all(|&b| b == 0));
    disk.write_all_at(512, &[8]).unwrap();
    disk.resize(512, ShrinkPolicy::AllowDataLoss).unwrap();
    disk.resize(M, ShrinkPolicy::Reject).unwrap();
    disk.read_exact_at(512, &mut out).unwrap();
    assert!(out.iter().all(|&b| b == 0));
}
#[test]
#[ignore = "requires independent native VBoxManage and qemu-img"]
fn virtualbox_and_qemu_accept_native_capacity_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let mut writer = VdiWriter::create_sparse(&path, 3 * M).unwrap();
    writer.write_all_at(0, &vec![1; M as usize]).unwrap();
    writer.write_all_at(M, &vec![2; M as usize]).unwrap();
    writer.write_all_at(2 * M, &vec![3; M as usize]).unwrap();
    writer.resize(129 * M, ShrinkPolicy::Reject).unwrap();
    writer.write_all_at(2 * M, &vec![3; M as usize]).unwrap();
    writer.resize(M + 512, ShrinkPolicy::AllowDataLoss).unwrap();
    writer.resize(3 * M, ShrinkPolicy::Reject).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let registry = dir.path().join("registry");
    std::fs::create_dir(&registry).unwrap();
    let ipc = format!(
        "virtdisk-resize-{}-{}",
        std::process::id(),
        dir.path().file_name().unwrap().to_string_lossy()
    );
    let vbox = |args: &[&std::ffi::OsStr]| {
        let output = std::process::Command::new("VBoxManage")
            .env("VBOX_USER_HOME", &registry)
            .env("VBOX_IPC_SOCKETID", &ipc)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    use std::ffi::OsStr;
    vbox(&[
        OsStr::new("showmediuminfo"),
        OsStr::new("disk"),
        path.as_os_str(),
    ]);
    let native = dir.path().join("native.raw");
    vbox(&[
        OsStr::new("clonemedium"),
        OsStr::new("disk"),
        path.as_os_str(),
        native.as_os_str(),
        OsStr::new("--format"),
        OsStr::new("RAW"),
    ]);
    let raw = dir.path().join("qemu.raw");
    let checked = std::process::Command::new("qemu-img")
        .args(["check", "-f", "vdi"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vdi", "-O", "raw"])
            .arg(&path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    let mut expected = vec![1; M as usize];
    expected.extend(vec![2; 512]);
    expected.extend(vec![0; M as usize - 512]);
    expected.extend(vec![0; M as usize]);
    assert_eq!(std::fs::read(native).unwrap(), expected);
    assert_eq!(std::fs::read(raw).unwrap(), expected);
}

#[test]
fn allocated_image_relocates_native_payload_when_map_arena_grows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let mut disk = VdiWriter::create_sparse(&path, 512).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.flush().unwrap();
    let old = std::fs::metadata(&path).unwrap().len();
    disk.resize(129 * M, ShrinkPolicy::Reject).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), old + M);
    let mut bytes = [0];
    disk.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [7]);
    disk.read_exact_at(512, &mut bytes).unwrap();
    assert_eq!(bytes, [0]);
    disk.write_all_at(128 * M, &[9]).unwrap();
    disk.flush().unwrap();
    drop(disk);
    let disk = VdiWriter::open(&path).unwrap();
    disk.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [7]);
    disk.read_exact_at(128 * M, &mut bytes).unwrap();
    assert_eq!(bytes, [9]);
}

#[test]
fn relocation_preserves_multiple_out_of_order_small_block_owners() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    drop(VdiWriter::create_sparse(&path, 512).unwrap());
    let mut header = std::fs::read(&path).unwrap();
    header[368..376].copy_from_slice(&1024u64.to_le_bytes());
    header[376..380].copy_from_slice(&512u32.to_le_bytes());
    header[384..388].copy_from_slice(&2u32.to_le_bytes());
    header[516..520].fill(255);
    std::fs::write(&path, header).unwrap();
    let mut disk = VdiWriter::open(&path).unwrap();
    disk.write_all_at(512, &[2]).unwrap();
    disk.write_all_at(0, &[1]).unwrap();
    disk.resize(129 * 512, ShrinkPolicy::Reject).unwrap();
    let mut out = [0];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [1]);
    disk.read_exact_at(512, &mut out).unwrap();
    assert_eq!(out, [2]);
    disk.flush().unwrap();
    drop(disk);
    let disk = VdiWriter::open(&path).unwrap();
    disk.read_exact_at(512, &mut out).unwrap();
    assert_eq!(out, [2]);
}

#[test]
fn allocated_arena_relocation_preserves_large_payload_without_whole_image_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let mut disk = VdiWriter::create_sparse(&path, 3 * M).unwrap();
    for (offset, value) in [(2 * M, 3), (0, 1), (M, 2)] {
        disk.write_all_at(offset, &vec![value; M as usize]).unwrap();
    }
    disk.resize(129 * M, ShrinkPolicy::Reject).unwrap();
    for index in 0..3 {
        let mut block = vec![0; M as usize];
        disk.read_exact_at(index * M, &mut block).unwrap();
        assert!(block.iter().all(|&b| b == index as u8 + 1));
    }
    disk.flush().unwrap();
    drop(disk);
    let disk = VdiWriter::open(&path).unwrap();
    let mut byte = [0];
    disk.read_exact_at(2 * M, &mut byte).unwrap();
    assert_eq!(byte, [3]);
}

#[test]
fn multi_stage_small_unit_relocation_preserves_data_and_preflights_map_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    drop(VdiWriter::create_sparse(&path, 512).unwrap());
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[376..380].copy_from_slice(&512u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    let mut disk = VdiWriter::open(&path).unwrap();
    disk.write_all_at(0, &[7]).unwrap();
    disk.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(
        disk.resize(((1 << 18) + 1) * 512, ShrinkPolicy::Reject)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    disk.resize(2 * M, ShrinkPolicy::Reject).unwrap();
    let mut out = [0];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [7]);
    disk.write_all_at(2 * M - 512, &[8]).unwrap();
    disk.flush().unwrap();
    drop(disk);
    let disk = VdiWriter::open(&path).unwrap();
    disk.read_exact_at(2 * M - 512, &mut out).unwrap();
    assert_eq!(out, [8]);
}
