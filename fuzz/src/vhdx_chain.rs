//! Harness-owned authorized native sector bitmap chains.
use virtdisk::{ParserLimits, ReadAt, Vhdx};
#[path = "../../tests/support/vhdx.rs"]
mod fixture_support;
use fixture_support::{M, fixture, put, put64};
#[path = "../../tests/support/vhdx_chain.rs"]
mod chain_support;
use chain_support::{child, encode_locator};
pub fn seeds() -> Vec<Vec<u8>> {
    let valid = child();
    let mut overlap = valid.clone();
    put64(&mut overlap, 2 * M + 4096 * 8, (4 * M as u64) | 6);
    let mut missing = valid.clone();
    put64(&mut missing, 2 * M + 4096 * 8, 0);
    let mut zero = valid.clone();
    put64(&mut zero, 2 * M + 8, 2);
    let native_locator = |absolute: &str| {
        let mut b = valid.clone();
        let loc = encode_locator(&[
            ("parent_linkage", "{00000000-0000-0000-0000-000000000000}"),
            ("relative_path", "parent.vhdx"),
            ("absolute_win32_path", absolute),
            (
                "volume_path",
                r"\\?\Volume{f8ca6cb5-12a2-470c-bfdf-fe35d8c84a63}\parent.vhdx",
            ),
        ]);
        put(&mut b, 3 * M + 32 + 5 * 32 + 20, loc.len() as u32);
        b[3 * M + 65616..3 * M + 65616 + loc.len()].copy_from_slice(&loc);
        b
    };
    let ordinary = native_locator(r"C:\images\parent.vhdx");
    let stream = native_locator(r"C:\images\parent.vhdx:stream");
    let device = native_locator(r"\\.\PhysicalDrive0");
    vec![valid, zero, overlap, missing, ordinary, stream, device]
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
    #[test]
    fn native_drive_locator_is_authorized_but_stream_and_device_hints_are_rejected() {
        let seeds = seeds();
        let image = open(&seeds[4]).unwrap();
        let mut bytes = [0; 4];
        image.read_exact_at(510, &mut bytes).unwrap();
        assert_eq!(bytes, [99, 99, 17, 17]);
        assert!(open(&seeds[5]).is_err());
        assert!(open(&seeds[6]).is_err());
    }
}
