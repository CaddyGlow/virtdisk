//! Native differencing fixtures shared by tests and fuzz seeds.
use super::fixture_support::{M, fixture, guid, put, put64};
fn locator() -> Vec<u8> {
    let pairs = [
        ("parent_linkage", "{00000000-0000-0000-0000-000000000000}"),
        ("relative_path", "parent.vhdx"),
    ];
    encode_locator(&pairs)
}
pub fn encode_locator(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut b = vec![0; 20 + pairs.len() * 12];
    b[..16].copy_from_slice(&guid("b7ef4ab09ed1814ab78925b8e9445913"));
    b[18..20].copy_from_slice(&(pairs.len() as u16).to_le_bytes());
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
pub fn child() -> Vec<u8> {
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
