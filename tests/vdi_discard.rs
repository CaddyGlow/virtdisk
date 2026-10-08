#![cfg(feature = "std")]
use std::sync::Arc;
use virtdisk::{DiscardPolicy, DiscardResult, RawDisk, ReadAt, Vdi, VdiWriter};
const M: u64 = 1 << 20;
#[test]
fn native_dynamic_discard_moves_last_owner_and_reclaims_physical_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let writer = VdiWriter::create_sparse(&path, 3 * M).unwrap();
    for i in 0..3 {
        writer
            .write_all_at(i * M, &vec![i as u8 + 1; M as usize])
            .unwrap();
    }
    writer.flush().unwrap();
    let before = std::fs::metadata(&path).unwrap().len();
    assert_eq!(
        writer
            .discard(0, M, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before - M);
    let mut out = [9; 4];
    writer.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0; 4]);
    writer.read_exact_at(2 * M, &mut out).unwrap();
    assert_eq!(out, [3; 4]);
    writer.write_all_at(17, &[8]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let image = Vdi::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    image.read_exact_at(17, &mut out).unwrap();
    assert_eq!(out, [8, 0, 0, 0]);
    image.read_exact_at(2 * M, &mut out).unwrap();
    assert_eq!(out, [3; 4]);
}
#[test]
fn native_overlay_discard_masks_inherited_data_without_changing_parent() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.vdi");
    let path = dir.path().join("child.vdi");
    let base = VdiWriter::create(&parent, 2 * M).unwrap();
    base.write_all_at(0, &vec![7; 2 * M as usize]).unwrap();
    base.flush().unwrap();
    drop(base);
    let original = std::fs::read(&parent).unwrap();
    let writer = VdiWriter::create_overlay(&path, &parent, &[]).unwrap();
    assert_eq!(
        writer
            .discard(0, M, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    let mut out = [1; 4];
    writer.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0; 4]);
    writer.read_exact_at(M, &mut out).unwrap();
    assert_eq!(out, [7; 4]);
    drop(writer);
    let image = Vdi::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
    image.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0; 4]);
    assert_eq!(std::fs::read(&parent).unwrap(), original);
}
#[test]
fn invalid_or_unsupported_discard_preserves_epochs_and_explicit_fallback_is_zeroed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixed.vdi");
    let writer = VdiWriter::create(&path, M).unwrap();
    writer.write_all_at(0, &[7; 512]).unwrap();
    writer.flush().unwrap();
    let original = std::fs::read(&path).unwrap();
    for (offset, length) in [(0, M), (1, M - 1), (M, 1), (u64::MAX, 1)] {
        assert!(
            writer
                .discard(offset, length, DiscardPolicy::RequireDeallocation)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    assert_eq!(
        writer
            .discard(0, 0, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Zeroed
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        writer
            .discard(3, 5, DiscardPolicy::AllowZeroFallback)
            .unwrap(),
        DiscardResult::Zeroed
    );
    let mut out = [0; 9];
    writer.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [7, 7, 7, 0, 0, 0, 0, 0, 7]);
}
#[test]
fn final_clipped_units_and_multiunit_discard_reuse_dense_zero_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.vdi");
    let writer = VdiWriter::create_sparse(&path, M + 512).unwrap();
    writer.write_all_at(0, &vec![2; M as usize]).unwrap();
    writer.write_all_at(M, &[3; 512]).unwrap();
    writer.flush().unwrap();
    assert_eq!(
        writer
            .discard(0, M + 512, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    assert_eq!(writer.len(), M + 512);
    let after = std::fs::metadata(&path).unwrap().len();
    assert!(after < M);
    let mut out = [1; 4];
    writer.read_exact_at(M, &mut out).unwrap();
    assert_eq!(out, [0; 4]);
    let identity = std::fs::read(&path).unwrap()[408..424].to_vec();
    assert_eq!(
        writer
            .discard(M, 512, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    assert_eq!(std::fs::read(&path).unwrap()[408..424], identity);
    writer.write_all_at(M + 1, &[8]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let image = Vdi::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    image.read_exact_at(M, &mut out).unwrap();
    assert_eq!(out, [0, 8, 0, 0]);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), after + M);
}
#[test]
fn unknown_tail_and_new_hardlink_are_rejected_before_modification_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let alias = dir.path().join("alias.vdi");
    let writer = VdiWriter::create_sparse(&path, M).unwrap();
    writer.write_all_at(0, &[7; 512]).unwrap();
    writer.flush().unwrap();
    let original = std::fs::read(&path).unwrap();
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(
        writer
            .discard(0, M, DiscardPolicy::RequireDeallocation)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    drop(writer);
    std::fs::remove_file(alias).unwrap();
    use virtdisk::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"foreign tail")
        .unwrap();
    let writer = VdiWriter::open(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(
        writer
            .discard(0, M, DiscardPolicy::RequireDeallocation)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}
#[test]
#[ignore = "requires independent native VBoxManage and qemu-img"]
fn virtualbox_and_qemu_accept_reclaimed_interior_discard() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let writer = VdiWriter::create_sparse(&path, 3 * M).unwrap();
    for i in 0..3 {
        writer
            .write_all_at(i * M, &vec![i as u8 + 1; M as usize])
            .unwrap();
    }
    writer.flush().unwrap();
    let before = std::fs::metadata(&path).unwrap().len();
    writer
        .discard(M, M, DiscardPolicy::RequireDeallocation)
        .unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before - M);
    let registry = dir.path().join("registry");
    std::fs::create_dir(&registry).unwrap();
    let ipc = format!(
        "virtdisk-discard-{}-{}",
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
    expected.extend(vec![0; M as usize]);
    expected.extend(vec![3; M as usize]);
    assert_eq!(std::fs::read(native).unwrap(), expected);
    assert_eq!(std::fs::read(raw).unwrap(), expected);
}
