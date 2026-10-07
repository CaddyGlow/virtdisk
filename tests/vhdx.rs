use std::{io, sync::Arc};
use virtdisk::{ParserLimits, ReadAt, Vhdx};
const M: usize = 1 << 20;
struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, o: u64, b: &mut [u8]) -> io::Result<()> {
        let s = usize::try_from(o).map_err(|_| io::ErrorKind::UnexpectedEof)?;
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
fn guid(s: &str) -> [u8; 16] {
    let mut b = [0; 16];
    for (i, c) in s.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        b[i] = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap();
    }
    b
}
fn checksum(b: &mut [u8]) {
    put(b, 4, 0);
    let mut crc = !0u32;
    for &v in b.iter() {
        crc ^= v as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    put(b, 4, !crc);
}
fn fixture() -> Vec<u8> {
    let mut b = vec![0; 6 * M];
    b[..8].copy_from_slice(b"vhdxfile");
    for (o, seq) in [(65536, 1), (131072, 2)] {
        b[o..o + 4].copy_from_slice(b"head");
        put64(&mut b, o + 8, seq);
        b[o + 66] = 1;
        put(&mut b, o + 68, M as u32);
        put64(&mut b, o + 72, M as u64);
        checksum(&mut b[o..o + 4096]);
    }
    for o in [196608, 262144] {
        b[o..o + 4].copy_from_slice(b"regi");
        put(&mut b, o + 8, 2);
        for (i, id) in [
            "6677c22d23f600429d64115e9bfd4a08",
            "06a27c8b90479a4bb8fe575f050f886e",
        ]
        .iter()
        .enumerate()
        {
            let at = o + 16 + i * 32;
            b[at..at + 16].copy_from_slice(&guid(id));
            put64(&mut b, at + 16, (2 + i) as u64 * M as u64);
            put(&mut b, at + 24, M as u32);
            put(&mut b, at + 28, 1);
        }
        checksum(&mut b[o..o + 65536]);
    }
    let o = 3 * M;
    b[o..o + 8].copy_from_slice(b"metadata");
    b[o + 10] = 5;
    for (i, (id, len, flags)) in [
        ("3767a1ca36fa434db3b633f0aa44e76b", 8, 4),
        ("2442a52f1bcd7648b2115dbed83bf4b8", 8, 6),
        ("ab12cabe e6b2234593efc309e000c746", 16, 6),
        ("1dbf41816fa90947ba47f233a8faab5f", 4, 6),
        ("c748a3cd5d4471449cc9e9885251c556", 4, 6),
    ]
    .iter()
    .enumerate()
    {
        let at = o + 32 + i * 32;
        let id = id.replace(' ', "");
        b[at..at + 16].copy_from_slice(&guid(&id));
        put(&mut b, at + 16, 65536 + i as u32 * 16);
        put(&mut b, at + 20, *len);
        put(&mut b, at + 24, *flags);
    }
    put(&mut b, o + 65536, M as u32);
    put64(&mut b, o + 65552, 2 * M as u64);
    put(&mut b, o + 65584, 512);
    put(&mut b, o + 65600, 4096);
    put64(&mut b, 2 * M, (4 * M as u64) | 6);
    put64(&mut b, 2 * M + 8, (5 * M as u64) | 6);
    b[4 * M..5 * M].fill(17);
    b[5 * M..].fill(23);
    b
}
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
