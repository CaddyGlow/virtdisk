use std::sync::Arc;
use virtdisk::{RawDisk, ReadAt, Vhdx, create_vhdx, recover_vhdx};
const M: usize = 1 << 20;
#[path = "support/bytes.rs"]
mod bytes;
use bytes::Bytes;

fn put(b: &mut [u8], o: usize, n: u32) {
    b[o..o + 4].copy_from_slice(&n.to_le_bytes());
}
fn put64(b: &mut [u8], o: usize, n: u64) {
    b[o..o + 8].copy_from_slice(&n.to_le_bytes());
}
fn crc(b: &mut [u8]) {
    put(b, 4, 0);
    let mut c = !0u32;
    for &v in b.iter() {
        c ^= v as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    put(b, 4, !c);
}
fn dirty(path: &std::path::Path) -> Vec<u8> {
    create_vhdx(path, &Bytes(vec![0x44; M])).unwrap();
    let mut b = std::fs::read(path).unwrap();
    let e = &mut b[M..M + 4096];
    e[..4].copy_from_slice(b"loge");
    put(e, 8, 4096);
    put64(e, 16, 1);
    put(e, 24, 1);
    e[32..48].fill(0x77);
    put64(e, 48, 5 * M as u64);
    put64(e, 56, 5 * M as u64);
    e[64..68].copy_from_slice(b"zero");
    put64(e, 72, 4096);
    put64(e, 80, 2 * M as u64);
    put64(e, 88, 1);
    crc(e);
    for o in [65536, 131072] {
        b[o + 48..o + 64].fill(0x77);
        crc(&mut b[o..o + 4096]);
    }
    b[2 * M..2 * M + 4096].fill(0xff);
    std::fs::write(path, &b).unwrap();
    b
}
#[test]
fn common_reader_replays_without_mutation_and_reports_physical_size() {
    use virtdisk::{Image, InspectImage, ReadRecoveryPolicy, ReaderOpenOptions};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vhdx");
    let mut original = dirty(&path);
    put64(&mut original, M + 56, 6 * M as u64);
    crc(&mut original[M..M + 4096]);
    std::fs::write(&path, &original).unwrap();
    assert!(Image::open_with_options(&path, &ReaderOpenOptions::default()).is_err());
    let options = ReaderOpenOptions::default().recovery_policy(ReadRecoveryPolicy::ReplayVhdxLog);
    let image = Image::open_with_options(&path, &options).unwrap();
    let mut bytes = vec![1; M];
    image.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|byte| *byte == 0));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let physical_size = original.len() as u64;
    assert_eq!(image.info().container_size, physical_size);
    assert_eq!(image.inspection().container_size, Some(physical_size));
    assert_eq!(image.inspection().container_set_size, Some(physical_size));
}
#[cfg(feature = "cli")]
#[test]
fn cli_immutable_log_replay_preserves_source_and_physical_inspection() {
    use std::process::Command;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vhdx");
    let mut original = dirty(&path);
    put64(&mut original, M + 56, 6 * M as u64);
    crc(&mut original[M..M + 4096]);
    std::fs::write(&path, &original).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--replay-vhdx-log", "info"])
        .arg(&path)
        .arg("vhdx")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains(&format!("\"container_size\":{}", original.len())));
    assert!(report.contains(&format!("\"container_set_size\":{}", original.len())));
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--parser-limit", "work=100000", "--replay-vhdx-log", "hash"])
        .arg(&path)
        .arg("vhdx")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = virtdisk::hash_image(&Bytes(vec![0; M]))
        .unwrap()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    assert_eq!(std::fs::read(&path).unwrap(), original);
}
#[cfg(feature = "cli")]
#[test]
fn cli_requires_explicit_recovery_before_mutation() {
    use std::process::Command;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vhdx");
    let original = dirty(&path);
    let denied = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("zero")
        .arg(&path)
        .args(["vhdx", "0", "512"])
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let recovered = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--recover", "zero"])
        .arg(&path)
        .args(["vhdx", "0", "512"])
        .output()
        .unwrap();
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let disk = Vhdx::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let mut bytes = vec![1; M];
    disk.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|byte| *byte == 0));
}
#[test]
fn native_recovery_is_clean_idempotent_and_preserves_log_bytes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("disk");
    let old = dirty(&p);
    recover_vhdx(&p).unwrap();
    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(&bytes[M..2 * M], &old[M..2 * M]);
    assert_ne!(&bytes[65536 + 16..65536 + 48], &old[65536 + 16..65536 + 48]);
    assert_eq!(&bytes[65536 + 48..65536 + 64], &[0; 16]);
    assert_eq!(&bytes[131072 + 48..131072 + 64], &[0; 16]);
    let disk = Vhdx::open(Arc::new(RawDisk::open(&p).unwrap())).unwrap();
    let mut out = [1; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0; 512]);
    recover_vhdx(&p).unwrap();
    assert_eq!(std::fs::read(p).unwrap(), bytes);
}

#[test]
fn common_open_requires_explicit_native_recovery_and_retains_lock() {
    use virtdisk::{
        ImageFormat, ImageWriter, RecoveryPolicy, RecoveryRequired, WriteAt, WriterOpenOptions,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vhdx");
    let original = dirty(&path);
    let error =
        ImageWriter::open_with_options(&path, ImageFormat::Vhdx, &WriterOpenOptions::default())
            .err()
            .unwrap();
    assert!(error.get_ref().unwrap().is::<RecoveryRequired>());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let options = WriterOpenOptions::default().recovery_policy(RecoveryPolicy::Recover);
    let writer = ImageWriter::open_with_options(&path, ImageFormat::Vhdx, &options).unwrap();
    let recovered = std::fs::read(&path).unwrap();
    assert_eq!(&recovered[M..2 * M], &original[M..2 * M]);
    assert!(ImageWriter::open_with_options(&path, ImageFormat::Vhdx, &options).is_err());
    let mut bytes = vec![0; M];
    writer.read_exact_at(0, &mut bytes).unwrap();
    // The native zero descriptor clears the BAT sector at 2 MiB, making the
    // entire logical payload unallocated rather than zeroing a payload sector.
    assert!(bytes.iter().all(|byte| *byte == 0));
    writer.flush().unwrap();
    drop(writer);
    drop(
        ImageWriter::open_with_options(&path, ImageFormat::Vhdx, &WriterOpenOptions::default())
            .unwrap(),
    );
    assert_eq!(std::fs::read(&path).unwrap(), recovered);
}
#[test]
fn invalid_recovered_metadata_and_lock_conflicts_never_mutate() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("disk");
    let mut bytes = dirty(&p);
    bytes[3 * M] = 0;
    std::fs::write(&p, &bytes).unwrap();
    assert!(recover_vhdx(&p).is_err());
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&p)
        .unwrap();
    f.try_lock().unwrap();
    assert!(recover_vhdx(&p).is_err());
    assert_eq!(std::fs::read(p).unwrap(), bytes);
}
#[test]
#[ignore = "requires independent qemu-img"]
fn qemu_checks_native_recovered_file_without_repair() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("disk");
    dirty(&p);
    let options =
        virtdisk::WriterOpenOptions::default().recovery_policy(virtdisk::RecoveryPolicy::Recover);
    drop(
        virtdisk::ImageWriter::open_with_options(&p, virtdisk::ImageFormat::Vhdx, &options)
            .unwrap(),
    );
    let raw = d.path().join("raw");
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "vhdx"])
            .arg(&p)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vhdx", "-O", "raw"])
            .arg(p)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(std::fs::read(raw).unwrap(), vec![0; M]);
}
