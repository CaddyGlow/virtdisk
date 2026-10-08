use std::sync::Arc;
use virtdisk::{RawDisk, ReadAt, Vhdx, VhdxWriter, create_vhdx};
const M: usize = 1 << 20;
#[path = "support/bytes.rs"]
mod bytes;
use bytes::Bytes;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("disk.vhdx");
    create_vhdx(&p, &Bytes(vec![17; M + 512])).unwrap();
    (d, p)
}
#[test]
fn updates_crossings_zeroes_and_reopen_with_new_uuid_epoch() {
    let (_d, p) = fixture();
    let initial = std::fs::read(&p).unwrap();
    let writer = VhdxWriter::open(&p).unwrap();
    assert_eq!(writer.len(), (M + 512) as u64);
    assert!(!writer.is_empty());
    assert_eq!(std::fs::read(&p).unwrap(), initial);
    writer.write_all_at(M as u64 - 2, &[1, 2, 3, 4]).unwrap();
    writer.write_zeroes(3, 13).unwrap();
    writer.flush().unwrap();
    let after = std::fs::read(&p).unwrap();
    assert_ne!(
        &after[65536 + 16..65536 + 48],
        &initial[65536 + 16..65536 + 48]
    );
    assert_eq!(
        &after[65536 + 16..65536 + 48],
        &after[131072 + 16..131072 + 48]
    );
    writer.write_all_at(0, &[7]).unwrap();
    writer.flush().unwrap();
    assert_eq!(
        &std::fs::read(&p).unwrap()[65536 + 16..65536 + 48],
        &after[65536 + 16..65536 + 48]
    );
    drop(writer);
    let disk = Vhdx::open(Arc::new(RawDisk::open(&p).unwrap())).unwrap();
    let mut b = [0; 4];
    disk.read_exact_at(M as u64 - 2, &mut b).unwrap();
    assert_eq!(b, [1, 2, 3, 4]);
    let mut b = [1; 13];
    disk.read_exact_at(3, &mut b).unwrap();
    assert_eq!(b, [0; 13]);
    let writer = VhdxWriter::open(&p).unwrap();
    writer.write_all_at(1, &[9]).unwrap();
    writer.flush().unwrap();
    assert_ne!(
        &std::fs::read(&p).unwrap()[65536 + 16..65536 + 48],
        &after[65536 + 16..65536 + 48]
    );
}
#[test]
fn bounded_reads_writes_and_exclusive_alias_lock() {
    let (_d, p) = fixture();
    let writer = VhdxWriter::open(&p).unwrap();
    let alias = p.with_file_name("alias");
    std::fs::hard_link(&p, &alias).unwrap();
    assert!(VhdxWriter::open(&p).is_err());
    assert!(VhdxWriter::open(alias).is_err());
    for at in [writer.len() + 1, u64::MAX] {
        assert!(writer.write_all_at(at, &[]).is_err());
        assert!(writer.read_exact_at(at, &mut []).is_err());
        assert!(writer.write_zeroes(at, 0).is_err());
    }
    assert!(writer.write_all_at(writer.len(), &[]).is_ok());
    assert!(writer.write_zeroes(writer.len() - 1, 2).is_err());
}
#[test]
fn sparse_profile_opens_and_torn_header_falls_back() {
    let (d, p) = fixture();
    let sparse = d.path().join("sparse");
    create_vhdx(&sparse, &Bytes(vec![0; 512])).unwrap();
    assert!(VhdxWriter::open(sparse).is_ok());
    let mut bytes = std::fs::read(&p).unwrap();
    bytes[131072 + 4] ^= 1;
    std::fs::write(&p, &bytes).unwrap();
    let writer = VhdxWriter::open(&p).unwrap();
    writer.write_all_at(0, &[22]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Vhdx::open(Arc::new(RawDisk::open(&p).unwrap())).unwrap();
    let mut b = [0];
    disk.read_exact_at(0, &mut b).unwrap();
    assert_eq!(b, [22]);
}
#[test]
fn concurrent_disjoint_payload_writes_are_serialized() {
    let (_d, p) = fixture();
    let writer = Arc::new(VhdxWriter::open(p).unwrap());
    std::thread::scope(|scope| {
        for index in 0..8 {
            let writer = writer.clone();
            scope.spawn(move || {
                writer.write_all_at(index * 64, &[index as u8; 64]).unwrap();
            });
        }
    });
    for index in 0..8 {
        let mut b = [0; 64];
        writer.read_exact_at(index * 64, &mut b).unwrap();
        assert_eq!(b, [index as u8; 64]);
    }
}
#[test]
#[ignore = "requires independent qemu-img"]
fn qemu_checks_and_reads_native_payload_updates() {
    let (_d, p) = fixture();
    let writer = VhdxWriter::open(&p).unwrap();
    writer.write_all_at(M as u64 - 2, &[1, 2, 3, 4]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let raw = p.with_extension("raw");
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
    let bytes = std::fs::read(raw).unwrap();
    assert_eq!(&bytes[M - 2..M + 2], &[1, 2, 3, 4]);
}

fn checksum_header(bytes: &mut [u8]) {
    bytes[4..8].fill(0);
    let mut crc = !0u32;
    for &b in bytes.iter() {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    bytes[4..8].copy_from_slice(&(!crc).to_le_bytes());
}
#[test]
fn invalid_ranges_and_exhausted_sequences_never_change_payload_or_headers() {
    let (_d, p) = fixture();
    let before = std::fs::read(&p).unwrap();
    let writer = VhdxWriter::open(&p).unwrap();
    assert!(writer.write_all_at(u64::MAX, &[1]).is_err());
    writer.write_all_at(writer.len(), &[]).unwrap();
    writer.write_zeroes(writer.len(), 0).unwrap();
    writer.flush().unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), before);
    drop(writer);
    let mut bytes = before;
    bytes[131072 + 8..131072 + 16].copy_from_slice(&u64::MAX.to_le_bytes());
    checksum_header(&mut bytes[131072..131072 + 4096]);
    std::fs::write(&p, &bytes).unwrap();
    let writer = VhdxWriter::open(&p).unwrap();
    assert!(writer.write_all_at(0, &[1]).is_err());
    assert_eq!(std::fs::read(p).unwrap(), bytes);
}
