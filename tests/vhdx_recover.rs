use std::{io, sync::Arc};
use virtdisk::{RawDisk, ReadAt, Vhdx, create_vhdx, recover_vhdx};
const M: usize = 1 << 20;
struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, o: u64, b: &mut [u8]) -> io::Result<()> {
        let s = o as usize;
        let e = s.checked_add(b.len()).ok_or(io::ErrorKind::UnexpectedEof)?;
        b.copy_from_slice(self.0.get(s..e).ok_or(io::ErrorKind::UnexpectedEof)?);
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
    recover_vhdx(&p).unwrap();
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
