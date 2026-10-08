#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use virtdisk::{ReadAt, Vhdx, VhdxWriter};
const M: u64 = 1 << 20;
fn fixture(sector: u32) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.vhdx");
    let child = dir.path().join("child.vhdx");
    let w = VhdxWriter::create(&parent, 2 * M).unwrap();
    w.write_all_at(0, &vec![7; (2 * M) as usize]).unwrap();
    w.flush().unwrap();
    drop(w);
    if sector == 4096 {
        let mut b = std::fs::read(&parent).unwrap();
        let meta = u64::from_le_bytes(b[196672..196680].try_into().unwrap()) as usize;
        b[meta + 65584..meta + 65588].copy_from_slice(&sector.to_le_bytes());
        std::fs::write(&parent, b).unwrap();
    }
    drop(VhdxWriter::create_overlay(&child, &parent, &[]).unwrap());
    (dir, parent, child)
}
#[test]
fn native_partial_sector_writes_preserve_inheritance_and_unaligned_sector_edges() {
    for sector in [512u32, 4096] {
        let (_dir, parent, child) = fixture(sector);
        let original = std::fs::read(&parent).unwrap();
        let writer = VhdxWriter::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
        writer.write_all_at(u64::from(sector) - 3, &[9; 8]).unwrap();
        writer.write_all_at(M + 17, &[8; 3]).unwrap();
        writer.flush().unwrap();
        let mut live = vec![0; (2 * M) as usize];
        writer.read_exact_at(0, &mut live).unwrap();
        drop(writer);
        let bytes = std::fs::read(&child).unwrap();
        let entry = u64::from_le_bytes(
            bytes[(2 * M) as usize..(2 * M + 8) as usize]
                .try_into()
                .unwrap(),
        );
        assert_eq!(entry & 7, 7, "write must publish native PARTIALLY_PRESENT");
        let entry2 = u64::from_le_bytes(
            bytes[(2 * M + 8) as usize..(2 * M + 16) as usize]
                .try_into()
                .unwrap(),
        );
        assert_eq!(entry2 & 7, 7);
        let image = Vhdx::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
        let mut out = vec![0; (2 * M) as usize];
        image.read_exact_at(0, &mut out).unwrap();
        assert_eq!(out, live);
        let mut expected = vec![7; (2 * M) as usize];
        expected[sector as usize - 3..sector as usize + 5].fill(9);
        expected[M as usize + 17..M as usize + 20].fill(8);
        assert_eq!(out, expected);
        assert_eq!(std::fs::read(&parent).unwrap(), original);
        let writer = VhdxWriter::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
        writer.write_all_at(3, &[6]).unwrap();
        writer.write_zeroes(u64::from(sector) * 3 + 1, 5).unwrap();
        writer.flush().unwrap();
        expected[3] = 6;
        expected[sector as usize * 3 + 1..sector as usize * 3 + 6].fill(0);
        writer.read_exact_at(0, &mut out).unwrap();
        assert_eq!(out, expected);
    }
}

#[test]
fn zero_masks_do_not_reveal_parent_and_shared_bitmap_keeps_other_owners() {
    use virtdisk::DiscardPolicy;
    let (_dir, parent, child) = fixture(512);
    let writer = VhdxWriter::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(17, &[9]).unwrap();
    writer.write_all_at(M + 29, &[8]).unwrap();
    writer
        .discard(0, M, DiscardPolicy::RequireDeallocation)
        .unwrap();
    writer.write_all_at(3, &[6]).unwrap();
    writer.flush().unwrap();
    let mut out = vec![1; (2 * M) as usize];
    writer.read_exact_at(0, &mut out).unwrap();
    drop(writer);
    assert_eq!(out[3], 6);
    assert!(
        out[..3]
            .iter()
            .chain(out[4..M as usize].iter())
            .all(|&b| b == 0)
    );
    assert_eq!(out[M as usize + 29], 8);
    assert_eq!(out[M as usize + 30], 7);
    let bytes = std::fs::read(&child).unwrap();
    let first = u64::from_le_bytes(
        bytes[(2 * M) as usize..(2 * M + 8) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        first & 7,
        6,
        "ZERO replacement must preserve all untouched zeroes"
    );
    let reader = Vhdx::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    let mut reopened = vec![0; (2 * M) as usize];
    reader.read_exact_at(0, &mut reopened).unwrap();
    assert_eq!(reopened, out);
}

#[test]
fn separate_chunk_bitmap_allocations_and_final_sector_stay_independent() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.vhdx");
    let child = dir.path().join("child.vhdx");
    let size = 4096 * M + 512;
    let base = VhdxWriter::create(&parent, size).unwrap();
    base.write_all_at(0, &[7; 512]).unwrap();
    base.write_all_at(4096 * M, &[8; 512]).unwrap();
    base.flush().unwrap();
    drop(base);
    let writer = VhdxWriter::create_overlay(&child, &parent, &[]).unwrap();
    writer.write_all_at(3, &[9]).unwrap();
    writer.write_all_at(size - 3, &[6; 3]).unwrap();
    writer.flush().unwrap();
    let mut first = [0; 512];
    let mut last = [0; 512];
    writer.read_exact_at(0, &mut first).unwrap();
    writer.read_exact_at(4096 * M, &mut last).unwrap();
    drop(writer);
    assert_eq!(first[3], 9);
    assert_eq!(first[4], 7);
    assert_eq!(last[508], 8);
    assert_eq!(&last[509..], &[6; 3]);
    let bytes = std::fs::read(&child).unwrap();
    let bitmap1 = u64::from_le_bytes(
        bytes[(2 * M + 4096 * 8) as usize..(2 * M + 4096 * 8 + 8) as usize]
            .try_into()
            .unwrap(),
    );
    let bitmap2 = u64::from_le_bytes(
        bytes[(2 * M + 8193 * 8) as usize..(2 * M + 8193 * 8 + 8) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(bitmap1 & 7, 6);
    assert_eq!(bitmap2 & 7, 6);
    assert_ne!(bitmap1 & !0xfffff, bitmap2 & !0xfffff);
    let image = Vhdx::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    let mut out = [0; 512];
    image.read_exact_at(4096 * M, &mut out).unwrap();
    assert_eq!(out, last);
}

#[test]
fn large_payload_block_write_crosses_bitmap_page_without_copying_parent_block() {
    use virtdisk::io::{Read, Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base.vhdx");
    let child = dir.path().join("child.vhdx");
    let boundary = 16 * M;
    let base = VhdxWriter::create(&parent, 32 * M).unwrap();
    base.write_all_at(boundary - 512, &[7; 1024]).unwrap();
    base.flush().unwrap();
    drop(base);
    let original = std::fs::read(&parent).unwrap();
    drop(VhdxWriter::create_overlay(&child, &parent, &[]).unwrap());
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&child)
        .unwrap();
    let mut field = [0; 8];
    file.seek(SeekFrom::Start(196672)).unwrap();
    file.read_exact(&mut field).unwrap();
    let metadata = u64::from_le_bytes(field);
    file.seek(SeekFrom::Start(metadata + 65536)).unwrap();
    file.write_all(&(32 * M as u32).to_le_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let writer = VhdxWriter::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(boundary - 3, &[9; 8]).unwrap();
    writer.flush().unwrap();
    let mut expected = [7; 1024];
    expected[509..517].fill(9);
    let mut actual = [0; 1024];
    writer.read_exact_at(boundary - 512, &mut actual).unwrap();
    assert_eq!(actual, expected);
    drop(writer);
    let mut file = std::fs::File::open(&child).unwrap();
    file.seek(SeekFrom::Start(2 * M)).unwrap();
    file.read_exact(&mut field).unwrap();
    assert_eq!(u64::from_le_bytes(field) & 7, 7);
    file.seek(SeekFrom::Start(2 * M + 128 * 8)).unwrap();
    file.read_exact(&mut field).unwrap();
    let bitmap = u64::from_le_bytes(field) & !0xfffff;
    let mut bits = [0; 2];
    file.seek(SeekFrom::Start(bitmap + 4095)).unwrap();
    file.read_exact(&mut bits).unwrap();
    assert_ne!(bits[0] & 128, 0);
    assert_ne!(bits[1] & 1, 0);
    let image = Vhdx::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    image.read_exact_at(boundary - 512, &mut actual).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(std::fs::read(&parent).unwrap(), original);
}
