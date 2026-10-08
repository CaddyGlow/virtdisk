//! Tiny split-sparse overwrite/allocation model; input never supplies paths.
pub(crate) fn seeds() -> Vec<Vec<u8>> {
    let allocated = [
        [0, 254, 1, 8, 0, 9],
        [1, 255, 1, 5, 0, 0],
        [2, 0, 0, 0, 0, 0],
        [3, 250, 1, 20, 0, 0],
        [4, 0, 0, 0, 0, 0],
        [6, 0, 0, 0, 0, 0],
        [7, 0, 0, 0, 0, 0],
        [5, 250, 1, 20, 0, 0],
    ];
    let hole = [
        [0, 254, 1, 8, 0, 9],
        [1, 255, 1, 5, 0, 0],
        [5, 250, 1, 20, 0, 0],
        [0, 100, 0, 17, 0, 7],
        [5, 101, 0, 3, 0, 0],
        [2, 0, 0, 0, 0, 0],
        [6, 0, 0, 0, 0, 0],
        [7, 0, 0, 0, 0, 0],
    ];
    [(4, allocated), (5, hole), (6, hole), (7, hole)]
        .into_iter()
        .map(|(mode, records)| {
            let mut data = vec![mode];
            for record in records {
                data.extend(record);
            }
            data
        })
        .collect()
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn run(_data: &[u8]) -> Vec<u8> {
    Vec::new()
}

#[cfg(target_os = "linux")]
pub(crate) fn run(data: &[u8]) -> Vec<u8> {
    use std::{fs, io, path::PathBuf};
    use virtdisk::{DiscardPolicy, DiscardResult, ReadAt, ShrinkPolicy, Vmdk, VmdkWriter};
    if data.len() > 49 || !matches!(data.first(), Some(4..=7)) {
        return Vec::new();
    }
    let hole = data[0] != 4;
    let missing_table = data[0] >= 6;
    let unsupported_table = data[0] == 7;
    let directory = tempfile::tempdir().unwrap();
    let descriptor = directory.path().join("disk.vmdk");
    let extents: Vec<PathBuf> = (0..2)
        .map(|i| directory.path().join(format!("disk-s{}.vmdk", i + 1)))
        .collect();
    for (index, extent) in extents.iter().enumerate() {
        let writer = VmdkWriter::create(extent, 512).unwrap();
        writer.write_all_at(0, &[41 + index as u8; 512]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let mut bytes = fs::read(extent).unwrap();
        let offset = u64::from_le_bytes(bytes[28..36].try_into().unwrap()) as usize * 512;
        let length = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize * 512;
        bytes[offset..offset + length].fill(0);
        if hole && index == 1 {
            for header in [48, 56] {
                let gd = u64::from_le_bytes(bytes[header..header + 8].try_into().unwrap()) as usize
                    * 512;
                if gd != 0 {
                    let gt =
                        u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
                    bytes[gt..gt + 4].fill(0);
                    if missing_table {
                        bytes[gd..gd + 4].fill(0);
                    }
                }
            }
        }
        if unsupported_table && index == 1 {
            bytes.extend([0; 512]);
        }
        fs::write(extent, bytes).unwrap();
    }
    fs::write(&descriptor, "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 1 SPARSE \"disk-s1.vmdk\"\nRW 1 SPARSE \"disk-s2.vmdk\"\n").unwrap();
    let participants: Vec<_> = std::iter::once(&descriptor).chain(&extents).collect();
    let snapshot = || {
        participants
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect::<Vec<_>>()
    };
    let assert_clean = || {
        let mut names: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        let mut expected: Vec<_> = participants
            .iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        expected.sort();
        assert_eq!(names, expected, "unexpected pending sidecar or participant");
    };
    let original = snapshot();
    for authorities in [&extents[..0], &extents[..1]] {
        assert_eq!(
            VmdkWriter::open_descriptor(&descriptor, authorities)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    assert_eq!(snapshot(), original);
    assert_clean();
    let mut model = vec![41; 512];
    model.extend(vec![if hole { 0 } else { 42 }; 512]);
    let mut writer = VmdkWriter::open_descriptor(&descriptor, &extents).unwrap();
    for record in data[1..].as_chunks::<6>().0.iter().take(8) {
        let offset = u16::from_le_bytes([record[1], record[2]]) as usize % 1537;
        let length = u16::from_le_bytes([record[3], record[4]]) as usize % 1025;
        let valid = offset
            .checked_add(length)
            .is_some_and(|end| end <= model.len());
        let supported = valid && (length == 0 || !unsupported_table || offset + length <= 512);
        let before = snapshot();
        match record[0] % 8 {
            operation @ (0 | 1 | 5) => {
                let result = match operation {
                    0 => writer.write_all_at(offset as u64, &vec![record[5]; length]),
                    1 => writer.write_zeroes(offset as u64, length as u64),
                    _ => writer
                        .discard(
                            offset as u64,
                            length as u64,
                            DiscardPolicy::AllowZeroFallback,
                        )
                        .map(|result| assert_eq!(result, DiscardResult::Zeroed)),
                };
                if supported {
                    result.unwrap();
                    model[offset..offset + length].fill(if operation == 0 { record[5] } else { 0 });
                } else {
                    let error = result.unwrap_err();
                    if valid {
                        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
                    }
                    assert_eq!(snapshot(), before);
                }
            }
            2 => {
                writer.flush().unwrap();
                drop(writer);
                let reader = Vmdk::open_descriptor(&descriptor, &extents).unwrap();
                let mut actual = vec![0; 1024];
                reader.read_exact_at(0, &mut actual).unwrap();
                assert_eq!(actual, model);
                drop(reader);
                writer = VmdkWriter::open_descriptor(&descriptor, &extents).unwrap();
            }
            3 => {
                let mut actual = vec![0; length];
                let result = writer.read_exact_at(offset as u64, &mut actual);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    assert_eq!(actual, model[offset..offset + length]);
                }
                assert_eq!(snapshot(), before);
            }
            4 => {
                assert!(writer.write_all_at(u64::MAX, &[record[5]]).is_err());
                assert!(writer.write_zeroes(1025, 0).is_err());
                assert_eq!(snapshot(), before);
            }
            6 => {
                assert_eq!(
                    writer
                        .discard(0, 1024, DiscardPolicy::RequireDeallocation)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Unsupported
                );
                assert_eq!(snapshot(), before);
            }
            _ => {
                assert_eq!(
                    writer
                        .resize(1536, ShrinkPolicy::AllowDataLoss)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Unsupported
                );
                assert_eq!(snapshot(), before);
            }
        }
        assert_clean();
        let mut actual = vec![0; 1024];
        writer.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, model);
    }
    writer.flush().unwrap();
    drop(writer);
    let reader = Vmdk::open_descriptor(&descriptor, &extents).unwrap();
    let mut actual = vec![0; 1024];
    reader.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, model);
    assert_clean();
    model
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn existing_table_hole_cross_write_allocates_and_preserves_zero_neighbors() {
        let mut data = vec![5];
        data.extend([0, 254, 1, 8, 0, 9]);
        data.extend([2, 0, 0, 0, 0, 0]);
        let mut expected = vec![41; 512];
        expected.extend(vec![0; 512]);
        expected[510..518].fill(9);
        assert_eq!(super::run(&data), expected);
    }
    #[test]
    fn zero_fallback_allocates_hole_before_partial_rewrite_and_reopen() {
        let mut data = vec![5];
        data.extend([5, 255, 1, 4, 0, 0]);
        data.extend([0, 1, 2, 3, 0, 7]);
        data.extend([2, 0, 0, 0, 0, 0]);
        data.extend([3, 250, 1, 20, 0, 0]);
        let mut expected = vec![41; 512];
        expected.extend(vec![0; 512]);
        expected[511..515].fill(0);
        expected[513..516].fill(7);
        assert_eq!(super::run(&data), expected);
    }
    #[test]
    fn cross_boundary_write_zero_and_reopen_match_independent_bytes() {
        let mut data = vec![4];
        data.extend([0, 254, 1, 8, 0, 9]);
        data.extend([1, 100, 0, 17, 0, 0]);
        data.extend([2, 0, 0, 0, 0, 0]);
        let mut expected = vec![41; 512];
        expected.extend(vec![42; 512]);
        expected[510..518].fill(9);
        expected[100..117].fill(0);
        assert_eq!(super::run(&data), expected);
    }
    #[test]
    fn unaligned_missing_table_refusals_preserve_all_files_and_allocated_fallback_updates_model() {
        let data = super::seeds().pop().unwrap();
        let mut expected = vec![41; 512];
        expected.extend(vec![0; 512]);
        expected[100..117].fill(7);
        expected[101..104].fill(0);
        assert_eq!(super::run(&data), expected);
    }
    #[test]
    fn missing_table_creation_then_write_zero_and_reopen_match_independent_bytes() {
        let mut data = vec![6];
        data.extend([0, 254, 1, 8, 0, 9]);
        data.extend([0, 1, 2, 3, 0, 7]);
        data.extend([5, 2, 2, 2, 0, 0]);
        data.extend([2, 0, 0, 0, 0, 0]);
        let mut expected = vec![41; 512];
        expected.extend(vec![0; 512]);
        expected[510..518].fill(9);
        expected[513..516].fill(7);
        expected[514..516].fill(0);
        assert_eq!(super::run(&data), expected);
    }
    #[test]
    fn split_seeds_append_after_existing_native_modes_and_dispatch() {
        let existing = crate::writable::seeds("vmdk-write");
        let all = crate::seeds("vmdk-write");
        assert_eq!(&all[..existing.len()], &existing);
        for seed in &all[existing.len()..] {
            crate::run("vmdk-write", seed).unwrap();
            assert_eq!(super::run(seed).len(), 1024);
        }
    }
    #[test]
    fn bounded_random_records_and_eight_cross_writes_remain_replayable() {
        for mode in [4, 5, 6, 7] {
            let mut data = vec![mode];
            let mut state = 0x932e_127a_5569_u64;
            for _ in 0..8 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                data.extend_from_slice(&state.to_le_bytes()[..6]);
            }
            assert_eq!(super::run(&data).len(), 1024);
            data.push(0);
            assert!(super::run(&data).is_empty());
        }
        for mode in [4, 6] {
            let mut data = vec![mode];
            for _ in 0..8 {
                data.extend([0, 254, 1, 8, 0, 9]);
            }
            let model = super::run(&data);
            assert_eq!(&model[510..518], &[9; 8]);
        }
    }
}
