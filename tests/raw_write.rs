use virtdisk::RawWriter;

#[test]
fn create_write_flush_reopen_and_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.raw");
    let writer = RawWriter::create(&path, 131_080).unwrap();
    assert_eq!(writer.len(), 131_080);
    writer.write_all_at(65_532, &[7; 12]).unwrap();
    writer.write_zeroes(65_535, 5).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let writer = RawWriter::open(&path).unwrap();
    let mut bytes = [0; 12];
    writer.read_exact_at(65_532, &mut bytes).unwrap();
    assert_eq!(bytes, [7, 7, 7, 0, 0, 0, 0, 0, 7, 7, 7, 7]);
    let mut unwritten = [1; 8];
    writer.read_exact_at(0, &mut unwritten).unwrap();
    assert_eq!(unwritten, [0; 8]);
}

#[test]
fn writes_and_reads_are_bounded_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let writer = RawWriter::create(dir.path().join("disk.raw"), 8).unwrap();
    writer.write_all_at(0, &[9; 8]).unwrap();
    assert!(writer.write_all_at(7, &[1, 2]).is_err());
    assert!(writer.write_all_at(u64::MAX, &[1]).is_err());
    assert!(writer.write_zeroes(7, 2).is_err());
    assert!(writer.write_zeroes(1, u64::MAX).is_err());
    assert!(writer.read_exact_at(7, &mut [0; 2]).is_err());
    assert!(writer.write_all_at(8, &[]).is_ok());
    assert!(writer.write_all_at(9, &[]).is_err());
    assert!(writer.write_zeroes(9, 0).is_err());
    let mut bytes = [0; 8];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [9; 8]);
}

#[test]
fn resize_discards_tail_and_growth_is_zeroed() {
    let dir = tempfile::tempdir().unwrap();
    let writer = RawWriter::create(dir.path().join("disk.raw"), 8).unwrap();
    writer.write_all_at(0, &[9; 8]).unwrap();
    writer.resize(4).unwrap();
    assert_eq!(writer.len(), 4);
    writer.resize(12).unwrap();
    let mut bytes = [0; 12];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [9, 9, 9, 9, 0, 0, 0, 0, 0, 0, 0, 0]);
    writer.resize(0).unwrap();
    assert!(writer.is_empty());
}

#[test]
fn create_never_overwrites_and_open_requires_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.raw");
    std::fs::write(&path, [1, 2, 3]).unwrap();
    assert!(RawWriter::create(&path, 8).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3]);
    assert!(RawWriter::open(dir.path()).is_err());
    assert!(RawWriter::open(dir.path().join("absent")).is_err());
}

#[test]
fn exclusive_writer_lock_survives_aliases_and_releases_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.raw");
    let writer = RawWriter::create(&path, 8).unwrap();
    let alias = dir.path().join("alias.raw");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(RawWriter::open(&path).is_err());
    assert!(RawWriter::open(&alias).is_err());
    drop(writer);
    assert!(RawWriter::open(&alias).is_ok());
}

#[test]
fn deterministic_mutations_match_memory_model() {
    let dir = tempfile::tempdir().unwrap();
    let writer = RawWriter::create(dir.path().join("disk.raw"), 257).unwrap();
    let mut model = vec![0; 257];
    let mut random = 0xabcde123_u64;
    for step in 0..400 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        if step % 13 == 0 {
            let size = (random % 513) as usize;
            writer.resize(size as u64).unwrap();
            model.resize(size, 0);
        } else {
            let offset = (random as usize) % (model.len() + 1);
            let count = ((random >> 32) as usize % 33).min(model.len() - offset);
            if step % 3 == 0 {
                writer.write_zeroes(offset as u64, count as u64).unwrap();
                model[offset..offset + count].fill(0);
            } else {
                let data = vec![step as u8; count];
                writer.write_all_at(offset as u64, &data).unwrap();
                model[offset..offset + count].copy_from_slice(&data);
            }
        }
        let mut actual = vec![0; model.len()];
        writer.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, model, "operation {step}");
    }
    writer.flush().unwrap();
}

#[test]
fn concurrent_disjoint_writes_preserve_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let writer = RawWriter::create(dir.path().join("disk.raw"), 16 * 64).unwrap();
    std::thread::scope(|scope| {
        for index in 0..16u64 {
            let writer = &writer;
            scope.spawn(move || {
                for _ in 0..50 {
                    writer.write_all_at(index * 64, &[index as u8; 64]).unwrap();
                    let mut data = [0; 64];
                    writer.read_exact_at(index * 64, &mut data).unwrap();
                    assert_eq!(data, [index as u8; 64]);
                }
            });
        }
    });
    let mut data = [0; 16 * 64];
    writer.read_exact_at(0, &mut data).unwrap();
    for (index, chunk) in data.as_chunks::<64>().0.iter().enumerate() {
        assert_eq!(chunk, &[index as u8; 64]);
    }
}
