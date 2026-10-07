use std::{io, sync::Arc};
use virtdisk::{RawDisk, ReadAt, Vhdx, create_vhdx};
struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, at: u64, out: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(at).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let end = start
            .checked_add(out.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        out.copy_from_slice(self.0.get(start..end).ok_or(io::ErrorKind::UnexpectedEof)?);
        Ok(())
    }
}
#[test]
fn exports_sparse_data_and_partial_block_and_unique_identity() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.vhdx");
    let b = dir.path().join("b.vhdx");
    let mut input = vec![0; 3 * 1024 * 1024 + 512];
    input[37] = 7;
    input[3 * 1024 * 1024 + 511] = 9;
    let source = Bytes(input.clone());
    create_vhdx(&a, &source).unwrap();
    create_vhdx(&b, &source).unwrap();
    let disk = Vhdx::open(Arc::new(RawDisk::open(&a).unwrap())).unwrap();
    let mut out = vec![0; input.len()];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(input, out);
    let file = std::fs::read(&a).unwrap();
    assert_eq!(file.len(), 6 * 1024 * 1024);

    let other = std::fs::read(&b).unwrap();
    assert_ne!(
        &file[65536 + 16..65536 + 32],
        &other[65536 + 16..65536 + 32]
    );
    assert_ne!(&file[65536 + 16..65536 + 32], &file[65536 + 32..65536 + 48]);
}
#[test]
fn existing_paths_and_invalid_capacity_are_rejected_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    std::fs::write(&path, b"keep").unwrap();
    assert!(create_vhdx(&path, &Bytes(vec![0; 512])).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    for len in [0, 1, 513] {
        let path = dir.path().join(format!("bad-{len}"));
        assert!(create_vhdx(&path, &Bytes(vec![0; len])).is_err());
        assert!(!path.exists());
    }
}
#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_checks_and_converts_native_vhdx() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let raw = dir.path().join("disk.raw");
    let mut bytes = vec![0; 1024 * 1024 + 512];
    bytes[73] = 8;
    bytes[1024 * 1024 + 511] = 99;
    create_vhdx(&path, &Bytes(bytes.clone())).unwrap();
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "vhdx"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vhdx", "-O", "raw"])
            .arg(&path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(std::fs::read(raw).unwrap(), bytes);
}

#[test]
fn empty_allocation_and_source_failure_do_not_publish_invalid_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zero.vhdx");
    create_vhdx(&path, &Bytes(vec![0; 512])).unwrap();
    let disk = Vhdx::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let mut out = [1; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0; 512]);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 4 * 1024 * 1024);
    struct Failed;
    impl ReadAt for Failed {
        fn len(&self) -> u64 {
            512
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::other("source failed"))
        }
    }
    let path = dir.path().join("failed.vhdx");
    assert!(create_vhdx(&path, &Failed).is_err());
    assert!(!path.exists());
}

#[test]
fn format_and_map_limits_are_checked_before_source_reads_or_creation() {
    struct Huge(u64);
    impl ReadAt for Huge {
        fn len(&self) -> u64 {
            self.0
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            panic!("invalid capacity must not trigger source reads")
        }
    }
    let dir = tempfile::tempdir().unwrap();
    for size in [65u64 << 40, 10u64 << 40, u64::MAX] {
        let path = dir.path().join(size.to_string());
        assert!(create_vhdx(&path, &Huge(size)).is_err());
        assert!(!path.exists());
    }
}
