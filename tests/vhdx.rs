use std::sync::Arc;
use virtdisk::io;
use virtdisk::{ParserLimits, ReadAt, Vhdx};
#[path = "support/bytes.rs"]
mod bytes;
#[path = "support/vhdx.rs"]
mod fixture_support;
use bytes::Bytes;
use fixture_support::{M, checksum, fixture, put, put64};
fn open(b: Vec<u8>) -> io::Result<Vhdx> {
    Vhdx::open(Arc::new(Bytes(b)))
}
#[test]
fn reads_payload_crossings_and_zero_states() {
    let mut b = fixture();
    put64(&mut b, 2 * M + 8, 2);
    let d = open(b).unwrap();
    let mut out = [0; 4];
    d.read_exact_at(M as u64 - 2, &mut out).unwrap();
    assert_eq!(out, [17, 17, 0, 0]);
    assert!(d.read_exact_at(2 * M as u64, &mut []).is_ok());
    assert!(d.read_exact_at(2 * M as u64 + 1, &mut []).is_err());
}
#[test]
fn redundant_roots_and_newest_header_are_respected() {
    let mut b = fixture();
    b[65536 + 4] ^= 1;
    assert!(open(b).is_ok());
    let mut b = fixture();
    b[196608 + 4] ^= 1;
    assert!(open(b).is_ok());
    let mut b = fixture();
    b[131072 + 48] = 1;
    checksum(&mut b[131072..131072 + 4096]);
    assert!(open(b).is_err());
}
#[test]
fn malformed_bat_metadata_and_parent_are_rejected() {
    for (at, value) in [
        (2 * M, (4 * M as u64) | 7),
        (2 * M + 8, (4 * M as u64) | 6),
        (2 * M, (3 * M as u64) | 6),
        (2 * M, (4 * M as u64) | 8),
        (3 * M + 65536 + 4, 2),
    ] {
        let mut b = fixture();
        put64(&mut b, at, value);
        assert!(open(b).is_err(), "{at}");
    }
    let mut b = fixture();
    b.pop();
    assert!(open(b).is_err());
}
#[test]
fn metadata_and_cache_budgets_are_enforced() {
    for l in [
        ParserLimits {
            metadata_bytes: 100,
            ..Default::default()
        },
        ParserLimits {
            cache_bytes: 4,
            ..Default::default()
        },
    ] {
        assert!(Vhdx::open_with_limits(Arc::new(Bytes(fixture())), l).is_err());
    }
}
#[cfg(feature = "std")]
#[test]
#[ignore = "requires independent qemu-img"]
fn qemu_fixed_and_dynamic_images_match_raw() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw");
    let mut bytes = vec![0; 3 * M + 512];
    bytes[4] = 9;
    bytes[3 * M] = 8;
    std::fs::write(&raw, &bytes).unwrap();
    for kind in ["fixed", "dynamic"] {
        let path = dir.path().join(kind);
        assert!(
            std::process::Command::new("qemu-img")
                .args(["convert", "-f", "raw", "-O", "vhdx", "-o"])
                .arg(format!("subformat={kind},block_size=1048576"))
                .arg(&raw)
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let disk = Vhdx::open(Arc::new(virtdisk::RawDisk::open(path).unwrap())).unwrap();
        let mut out = vec![0; bytes.len()];
        disk.read_exact_at(0, &mut out).unwrap();
        assert_eq!(out, bytes);
    }
}

#[test]
fn interleaved_sector_bitmap_slots_are_not_logical_payloads() {
    let mut b = fixture();
    put64(&mut b, 3 * M + 65552, 4097 * M as u64);
    let disk = open(b.clone()).unwrap();
    assert_eq!(disk.len(), 4097 * M as u64);
    let mut out = [1; 512];
    disk.read_exact_at(4096 * M as u64, &mut out).unwrap();
    assert_eq!(out, [0; 512]);
    put64(&mut b, 2 * M + 4096 * 8, 6);
    assert!(open(b).is_err());
}
#[test]
fn both_bad_roots_conflicting_region_copies_and_required_metadata_fail() {
    let mut b = fixture();
    b[65536 + 4] ^= 1;
    b[131072 + 4] ^= 1;
    assert!(open(b).is_err());
    let mut b = fixture();
    b[196608 + 4] ^= 1;
    b[262144 + 4] ^= 1;
    assert!(open(b).is_err());
    let mut b = fixture();
    put64(&mut b, 262144 + 16 + 16, 5 * M as u64);
    checksum(&mut b[262144..262144 + 65536]);
    assert!(open(b).is_err());
    let mut b = fixture();
    b[3 * M + 32] = 0;
    assert!(open(b).is_err());
    let mut b = fixture();
    put(&mut b, 3 * M + 32 + 16, 32);
    assert!(open(b).is_err());
    let mut b = fixture();
    b[131072 + 66] = 2;
    checksum(&mut b[131072..131072 + 4096]);
    assert!(open(b).is_err());
}
