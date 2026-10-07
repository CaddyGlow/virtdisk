//! Harness-owned authorized native sector bitmap chains.
use virtdisk::{ParserLimits, ReadAt, Vhdx};
const M: usize = 1 << 20;
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
fn locator() -> Vec<u8> {
    let pairs = [
        ("parent_linkage", "{00000000-0000-0000-0000-000000000000}"),
        ("relative_path", "parent.vhdx"),
    ];
    let mut b = vec![0; 44];
    b[..16].copy_from_slice(&guid("b7ef4ab09ed1814ab78925b8e9445913"));
    b[18] = 2;
    for (i, (key, value)) in pairs.iter().enumerate() {
        for (j, s) in [key, value].iter().enumerate() {
            let start = b.len() as u32;
            let raw: Vec<_> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let n = raw.len() as u16;
            let at = 20 + i * 12;
            put(&mut b, at + j * 4, start);
            b[at + 8 + j * 2..at + 10 + j * 2].copy_from_slice(&n.to_le_bytes());
            b.extend(raw);
        }
    }
    b
}
fn child() -> Vec<u8> {
    let mut b = fixture();
    b.resize(7 * M, 0);
    b[3 * M + 10] = 6;
    put(&mut b, 3 * M + 65540, 2);
    let e = 3 * M + 32 + 5 * 32;
    b[e..e + 16].copy_from_slice(&guid("2d5fd3a80bb34d45abf7d3d84834ab0c"));
    let loc = locator();
    put(&mut b, e + 16, 65616);
    put(&mut b, e + 20, loc.len() as u32);
    put(&mut b, e + 24, 4);
    b[3 * M + 65616..3 * M + 65616 + loc.len()].copy_from_slice(&loc);
    b[2 * M..3 * M].fill(0);
    put64(&mut b, 2 * M, (4 * M as u64) | 7);
    put64(&mut b, 2 * M + 4096 * 8, (6 * M as u64) | 6);
    b[4 * M..5 * M].fill(99);
    b[6 * M] = 1;
    b
}

pub fn seeds() -> Vec<Vec<u8>> {
    let valid = child();
    let mut overlap = valid.clone();
    put64(&mut overlap, 2 * M + 4096 * 8, (4 * M as u64) | 6);
    let mut missing = valid.clone();
    put64(&mut missing, 2 * M + 4096 * 8, 0);
    let mut zero = valid.clone();
    put64(&mut zero, 2 * M + 8, 2);
    vec![valid, zero, overlap, missing]
}
pub fn open(data: &[u8]) -> std::io::Result<Vhdx> {
    let directory = tempfile::tempdir()?;
    let parent = directory.path().join("parent.vhdx");
    let path = directory.path().join("child.vhdx");
    std::fs::write(&parent, fixture())?;
    std::fs::write(&path, data)?;
    let limits = ParserLimits {
        metadata_bytes: 8 << 20,
        work_items: 100000,
        recursion_depth: 2,
        ..ParserLimits::default()
    };
    Vhdx::open_chain_with_limits(&path, std::slice::from_ref(&parent), limits)
}
pub fn run(data: &[u8]) {
    if data.len() > 8 << 20 {
        return;
    }
    let Ok(image) = open(data) else {
        return;
    };
    let mut bytes = [0; 4096];
    for offset in [
        0,
        510,
        511,
        512,
        4095,
        1048575,
        1048576,
        image.len().saturating_sub(4096),
    ] {
        let count = image.len().saturating_sub(offset).min(bytes.len() as u64) as usize;
        let _ = image.read_exact_at(offset, &mut bytes[..count]);
    }
    let mut visits = 0;
    let _ = image.visit_extents(&mut |_| {
        visits += 1;
        if visits > 32 {
            Err(std::io::ErrorKind::Interrupted.into())
        } else {
            Ok(())
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_partial_bitmap_inherits_and_malformed_ownership_is_rejected() {
        let seeds = seeds();
        let image = open(&seeds[0]).unwrap();
        let mut bytes = [0; 4];
        image.read_exact_at(510, &mut bytes).unwrap();
        assert_eq!(bytes, [99, 99, 17, 17]);
        assert!(open(&seeds[2]).is_err());
        assert!(open(&seeds[3]).is_err());
        let image = open(&seeds[1]).unwrap();
        image.read_exact_at(M as u64, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 4]);
    }
}
