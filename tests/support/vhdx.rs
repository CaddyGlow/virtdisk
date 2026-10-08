//! Hand-built VHDX fixtures, independent of the library encoder and checksum.
pub const M: usize = 1 << 20;
pub fn put(b: &mut [u8], o: usize, n: u32) {
    b[o..o + 4].copy_from_slice(&n.to_le_bytes());
}
pub fn put64(b: &mut [u8], o: usize, n: u64) {
    b[o..o + 8].copy_from_slice(&n.to_le_bytes());
}
pub fn guid(s: &str) -> [u8; 16] {
    let mut b = [0; 16];
    for (i, c) in s.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        b[i] = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap();
    }
    b
}
pub fn checksum(b: &mut [u8]) {
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
pub fn fixture() -> Vec<u8> {
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
