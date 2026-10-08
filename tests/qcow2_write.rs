#![cfg(feature = "std")]
use std::sync::Arc;
use virtdisk::io;
use virtdisk::{Qcow2, RawDisk, ReadAt, create_qcow2};

struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        let start = offset as usize;
        dst.copy_from_slice(
            self.0
                .get(start..start + dst.len())
                .ok_or(io::ErrorKind::UnexpectedEof)?,
        );
        Ok(())
    }
}

#[test]
fn exports_partial_clusters_and_multiple_l2_tables_with_valid_ownership() {
    for size in [0, 512, 65536 + 512, 8192 * 65536 + 512] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.qcow2");
        // The large case uses a sparse logical source to avoid a 512 MiB RAM fixture.
        let source = Pattern(size);
        create_qcow2(&path, &source).unwrap();
        let image = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
        assert_eq!(image.len(), size);
        image.validate_active_mapping().unwrap();
        for offset in [0, size / 2, size.saturating_sub(19)] {
            let count = 19.min(size - offset) as usize;
            let mut actual = vec![0; count];
            let mut expected = vec![0; count];
            source.read_exact_at(offset, &mut expected).unwrap();
            image.read_exact_at(offset, &mut actual).unwrap();
            assert_eq!(actual, expected);
        }
    }
}
struct Pattern(u64);
impl ReadAt for Pattern {
    fn len(&self) -> u64 {
        self.0
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        if offset
            .checked_add(dst.len() as u64)
            .is_none_or(|end| end > self.0)
        {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        for (i, byte) in dst.iter_mut().enumerate() {
            *byte = ((offset + i as u64) % 251) as u8;
        }
        Ok(())
    }
}

#[test]
fn export_refuses_existing_paths_and_excessive_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing");
    std::fs::write(&path, [9]).unwrap();
    assert!(create_qcow2(&path, &Bytes(vec![1])).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), [9]);
    let absent = dir.path().join("absent");
    assert!(create_qcow2(&absent, &Pattern(u64::MAX)).is_err());
    assert!(!absent.exists());
    assert!(create_qcow2(&absent, &Pattern(513)).is_err());
    assert!(!absent.exists());
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_checks_and_converts_exported_image() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.qcow2");
    let raw = dir.path().join("image.raw");
    let source = Bytes((0..200_192).map(|i| (i % 251) as u8).collect());
    create_qcow2(&path, &source).unwrap();
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    let actual = std::fs::read(raw).unwrap();
    assert!(actual == source.0);
}

#[test]
fn source_read_failure_propagates_without_claiming_success() {
    struct Failing;
    impl ReadAt for Failing {
        fn len(&self) -> u64 {
            2 * 65536
        }
        fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
            if offset >= 65536 {
                return Err(io::Error::other("injected source failure"));
            }
            out.fill(7);
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.qcow2");
    let error = create_qcow2(&path, &Failing).unwrap_err();
    assert_eq!(error.to_string(), "injected source failure");
}
