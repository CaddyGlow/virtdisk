#![cfg(feature = "std")]
//! Independent recovery review regressions.
#[cfg(target_os = "linux")]
#[test]
fn published_clean_qcow2_transaction_cannot_be_bypassed_through_hardlink_alias() {
    use sha2::{Digest, Sha256};
    use virtdisk::Qcow2Writer;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");
    let alias = dir.path().join("alias.qcow2");
    drop(Qcow2Writer::create(&path, 65536).unwrap());
    let original = std::fs::read(&path).unwrap();
    let offset = 5 * 65536u64;
    // This is the durable publication boundary before the native dirty-bit write.
    let mut journal = Vec::new();
    journal.extend_from_slice(b"VDQCJ001");
    journal.extend_from_slice(&(original.len() as u64).to_be_bytes());
    journal.extend_from_slice(&(original.len() as u64).to_be_bytes());
    journal.extend_from_slice(&Sha256::digest(&original));
    journal.extend_from_slice(&1u64.to_be_bytes());
    journal.extend_from_slice(&offset.to_be_bytes());
    journal.extend_from_slice(&512u64.to_be_bytes());
    journal.extend_from_slice(&512u64.to_be_bytes());
    journal.extend_from_slice(&original[offset as usize..offset as usize + 512]);
    journal.extend_from_slice(&[7; 512]);
    let checksum = Sha256::digest(&journal);
    journal.extend_from_slice(&checksum);
    std::fs::write(
        path.with_file_name("disk.qcow2.virtdisk-qcow2-journal"),
        journal,
    )
    .unwrap();
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(
        Qcow2Writer::open(&alias).is_err(),
        "a clean image with a durable journal must not become writable through another file name"
    );
}

#[test]
fn vhdx_recovery_rejects_log_updates_to_payload_header_or_log() {
    use std::sync::Arc;
    use virtdisk::io;
    use virtdisk::{ReadAt, Vhdx, create_vhdx};
    const M: usize = 1 << 20;
    struct Bytes(Vec<u8>);
    impl ReadAt for Bytes {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_exact_at(&self, o: u64, b: &mut [u8]) -> io::Result<()> {
            b.copy_from_slice(
                self.0
                    .get(o as usize..o as usize + b.len())
                    .ok_or(io::ErrorKind::UnexpectedEof)?,
            );
            Ok(())
        }
    }
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    create_vhdx(&path, &Bytes(vec![3; M])).unwrap();
    let original = std::fs::read(&path).unwrap();
    for target in [65536, M, 4 * M] {
        let mut image = original.clone();
        let log = &mut image[M..M + 4096];
        log[..4].copy_from_slice(b"loge");
        put(log, 8, 4096);
        put64(log, 16, 1);
        put(log, 24, 1);
        log[32..48].fill(7);
        put64(log, 48, 5 * M as u64);
        put64(log, 56, 5 * M as u64);
        log[64..68].copy_from_slice(b"zero");
        put64(log, 72, 4096);
        put64(log, 80, target as u64);
        put64(log, 88, 1);
        crc(log);
        for offset in [65536, 131072] {
            image[offset + 48..offset + 64].fill(7);
            crc(&mut image[offset..offset + 4096]);
        }
        assert!(
            Vhdx::open_recovered(Arc::new(Bytes(image))).is_err(),
            "target {target}"
        );
    }
}
