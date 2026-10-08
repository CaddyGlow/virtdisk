use std::{io, sync::Arc};
use virtdisk::{RawDisk, ReadAt, Vdi, create_vdi};
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
fn export_streams_large_blocks_in_bounded_reads() {
    struct Bounded(Bytes);
    impl ReadAt for Bounded {
        fn len(&self) -> u64 {
            self.0.len()
        }
        fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
            if destination.len() > 65536 {
                return Err(io::Error::other("export read exceeds 64 KiB"));
            }
            self.0.read_exact_at(offset, destination)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("streamed.vdi");
    let mut bytes = vec![0; 3 * 1024 * 1024 + 512];
    for offset in [65535, 65536, 1024 * 1024 - 1, bytes.len() - 1] {
        bytes[offset] = 91;
    }
    create_vdi(&path, &Bounded(Bytes(bytes.clone()))).unwrap();
    let disk = Vdi::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let mut actual = vec![0; bytes.len()];
    disk.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, bytes);
    let native = std::fs::read(path).unwrap();
    assert_eq!(u32::from_le_bytes(native[388..392].try_into().unwrap()), 2);
    assert!(
        native[native.len() - (1024 * 1024 - 512)..]
            .iter()
            .all(|&b| b == 0)
    );
}
#[test]
fn failed_streaming_payload_leaves_metadata_uninstalled() {
    struct FailedPayload(std::sync::atomic::AtomicUsize);
    impl ReadAt for FailedPayload {
        fn len(&self) -> u64 {
            1024 * 1024
        }
        fn read_exact_at(&self, _: u64, destination: &mut [u8]) -> io::Result<()> {
            // Sixteen reads scan the block; fail on the second payload chunk.
            if self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 17 {
                return Err(io::Error::other("payload read failed"));
            }
            destination.fill(7);
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed.vdi");
    let error = create_vdi(
        &path,
        &FailedPayload(std::sync::atomic::AtomicUsize::new(0)),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "payload read failed");
    let native = std::fs::read(&path).unwrap();
    assert!(native[..1024].iter().all(|&byte| byte == 0));
    assert!(native[1024..1024 + 65536].iter().all(|&byte| byte == 7));
    assert!(Vdi::open(Arc::new(RawDisk::open(path).unwrap())).is_err());
}
#[test]
fn exports_sparse_data_and_partial_block_and_unique_identity() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.vdi");
    let b = dir.path().join("b.vdi");
    let mut input = vec![0; 3 * 1024 * 1024 + 512];
    input[37] = 7;
    input[3 * 1024 * 1024 + 511] = 9;
    let source = Bytes(input.clone());
    create_vdi(&a, &source).unwrap();
    create_vdi(&b, &source).unwrap();
    let disk = Vdi::open(Arc::new(RawDisk::open(&a).unwrap())).unwrap();
    let mut out = vec![0; input.len()];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(input, out);
    let file = std::fs::read(&a).unwrap();
    assert_eq!(u32::from_le_bytes(file[388..392].try_into().unwrap()), 2);
    assert!(file.len() < input.len());
    let other = std::fs::read(&b).unwrap();
    assert_ne!(&file[392..408], &other[392..408]);
    assert_ne!(&file[392..408], &file[408..424]);
}
#[test]
fn existing_paths_and_invalid_capacity_are_rejected_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    std::fs::write(&path, b"keep").unwrap();
    assert!(create_vdi(&path, &Bytes(vec![0; 512])).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    for len in [0, 1, 513] {
        let path = dir.path().join(format!("bad-{len}"));
        assert!(create_vdi(&path, &Bytes(vec![0; len])).is_err());
        assert!(!path.exists());
    }
}
#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_checks_and_converts_native_vdi() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vdi");
    let raw = dir.path().join("disk.raw");
    let mut bytes = vec![0; 1024 * 1024 + 512];
    bytes[73] = 8;
    bytes[1024 * 1024 + 511] = 99;
    create_vdi(&path, &Bytes(bytes.clone())).unwrap();
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "vdi"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
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
    assert_eq!(std::fs::read(raw).unwrap(), bytes);
}

#[test]
fn empty_allocation_and_source_failure_do_not_publish_invalid_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zero.vdi");
    create_vdi(&path, &Bytes(vec![0; 512])).unwrap();
    let disk = Vdi::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let mut out = [1; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0; 512]);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 1024);
    struct Failed;
    impl ReadAt for Failed {
        fn len(&self) -> u64 {
            512
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::other("source failed"))
        }
    }
    let path = dir.path().join("failed.vdi");
    assert!(create_vdi(&path, &Failed).is_err());
    assert!(!path.exists());
}
