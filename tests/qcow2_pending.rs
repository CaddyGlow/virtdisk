#![cfg(feature = "std")]
use std::sync::Arc;
use virtdisk::io;
use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt};

#[test]
fn clean_pending_sidecar_requires_recovery_without_readonly_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    drop(Qcow2Writer::create(&path, 65536).unwrap());
    let mut journal = path.as_os_str().to_owned();
    journal.push(".virtdisk-qcow2-journal");
    std::fs::write(&journal, b"incomplete publication").unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap()))
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(error.to_string().contains("recovery"));
    assert!(Qcow2::open_chain(&path, &[]).is_err());
    assert!(std::fs::read(&path).unwrap() == before);
    struct Memory(Vec<u8>);
    impl ReadAt for Memory {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
            dst.copy_from_slice(&self.0[offset as usize..offset as usize + dst.len()]);
            Ok(())
        }
    }
    Qcow2::open(Arc::new(Memory(before)))
        .unwrap()
        .validate_active_mapping()
        .unwrap();
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias.qcow2");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let error = Qcow2::open(Arc::new(RawDisk::open(alias).unwrap()))
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }
}
