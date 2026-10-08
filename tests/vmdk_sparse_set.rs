#![cfg(target_os = "linux")]
use std::{fs, io};
use virtdisk::{RawWriter, ReadAt, Vmdk, VmdkWriter};
#[path = "../src/test_sync.rs"]
mod process_boundary;

fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Vec<std::path::PathBuf>,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let mut extents = Vec::new();
    for index in 0..2 {
        let extent = directory.path().join(format!("disk-s{}.vmdk", index + 1));
        let writer = VmdkWriter::create(&extent, 65536).unwrap();
        writer
            .write_all_at(0, &vec![index as u8 + 41; 65536])
            .unwrap();
        writer.flush().unwrap();
        drop(writer);
        let mut bytes = fs::read(&extent).unwrap();
        let offset = u64::from_le_bytes(bytes[28..36].try_into().unwrap()) as usize * 512;
        let length = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize * 512;
        bytes[offset..offset + length].fill(0);
        fs::write(&extent, bytes).unwrap();
        extents.push(extent);
    }
    fs::write(&path,"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"disk-s1.vmdk\"\nRW 128 SPARSE \"disk-s2.vmdk\"\n").unwrap();
    (directory, path, extents)
}

#[test]
fn allocated_split_sparse_cross_boundary_write_reopens_exactly() {
    let _boundary = process_boundary::writer_test();
    let (_directory, path, extents) = fixture();
    let before = fs::read(&path).unwrap();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    assert!(writer.is_descriptor());
    for file in std::iter::once(&path).chain(&extents) {
        assert_eq!(
            RawWriter::open(file).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    writer.write_all_at(65529, &[9; 23]).unwrap();
    let epoch = fs::read(&path).unwrap();
    writer.write_zeroes(101, 17).unwrap();
    assert_eq!(fs::read(&path).unwrap(), epoch);
    writer.flush().unwrap();
    assert_ne!(fs::read(&path).unwrap(), before);
    let mut expected = vec![41; 65536];
    expected.extend(vec![42; 65536]);
    expected[65529..65552].fill(9);
    expected[101..118].fill(0);
    let mut actual = vec![0; 131072];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, expected);
    drop(writer);
    Vmdk::open_descriptor(&path, &extents)
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
    VmdkWriter::open_descriptor(&path, &extents)
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
}

fn clear_first_mapping(extent: &std::path::Path) {
    let mut bytes = fs::read(extent).unwrap();
    for at in [48, 56] {
        let directory = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize * 512;
        if directory != 0 {
            let table = u32::from_le_bytes(bytes[directory..directory + 4].try_into().unwrap())
                as usize
                * 512;
            bytes[table..table + 4].fill(0);
        }
    }
    fs::write(extent, bytes).unwrap();
}

#[test]
fn split_sparse_hole_allocation_initializes_neighbors_and_crosses_extents() {
    let _boundary = process_boundary::writer_test();
    let (_directory, path, extents) = fixture();
    for extent in &extents {
        clear_first_mapping(extent);
    }
    let before = extents
        .iter()
        .map(|extent| fs::metadata(extent).unwrap().len())
        .collect::<Vec<_>>();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_all_at(65529, &[9; 23]).unwrap();
    writer.write_all_at(11, &[7; 19]).unwrap();
    writer.write_zeroes(65534, 7).unwrap();
    writer.flush().unwrap();
    let mut expected = vec![0; 131072];
    expected[65529..65552].fill(9);
    expected[11..30].fill(7);
    expected[65534..65541].fill(0);
    let mut actual = vec![0; 131072];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, expected);
    for (extent, before) in extents.iter().zip(before) {
        assert_eq!(fs::metadata(extent).unwrap().len(), before + 65536);
    }
    drop(writer);
    Vmdk::open_descriptor(&path, &extents)
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
    VmdkWriter::open_descriptor(&path, &extents)
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn missing_table_creation_precedes_first_payload_in_empty_extent() {
    let _boundary = process_boundary::writer_test();
    let (_directory, path, extents) = fixture();
    let mut bytes = fs::read(&extents[1]).unwrap();
    for at in [48, 56] {
        let gd = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize * 512;
        if gd != 0 {
            bytes[gd..gd + 4].fill(0);
        }
    }
    fs::write(&extents[1], bytes).unwrap();
    let eof = fs::metadata(&extents[1]).unwrap().len();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_all_at(65529, &[9; 23]).unwrap();
    writer.write_all_at(65553, &[7; 19]).unwrap();
    writer.write_zeroes(65534, 7).unwrap();
    writer.flush().unwrap();
    let mut expected = vec![41; 65536];
    expected.extend(vec![0; 65536]);
    expected[65529..65552].fill(9);
    expected[65553..65572].fill(7);
    expected[65534..65541].fill(0);
    let mut actual = vec![0; 131072];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(fs::metadata(&extents[1]).unwrap().len(), eof + 131072);
    drop(writer);
    Vmdk::open_descriptor(&path, &extents)
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
    VmdkWriter::open_descriptor(&path, &extents)
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
}

#[test]
#[ignore = "requires independent qemu-img split sparse oracle"]
fn split_sparse_allocated_overwrite_remains_qemu_readable() {
    let _boundary = process_boundary::subprocess_test();
    let (directory, path, extents) = fixture();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_all_at(65529, &[9; 23]).unwrap();
    writer.write_zeroes(101, 17).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let output = directory.path().join("after.raw");
    let result = std::process::Command::new("qemu-img")
        .args(["convert", "-f", "vmdk", "-O", "raw"])
        .arg(&path)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut expected = vec![41; 65536];
    expected.extend(vec![42; 65536]);
    expected[65529..65552].fill(9);
    expected[101..118].fill(0);
    assert_eq!(fs::read(output).unwrap(), expected);
}

#[test]
fn late_busy_extent_authorization_and_missing_tables_refuse_unchanged() {
    let _boundary = process_boundary::writer_test();
    let (_directory, path, extents) = fixture();
    let originals = std::iter::once(&path)
        .chain(&extents)
        .map(|p| fs::read(p).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        VmdkWriter::open_descriptor(&path, &extents[..1])
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let busy = RawWriter::open(&extents[1]).unwrap();
    assert_eq!(
        VmdkWriter::open_descriptor(&path, &extents)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(RawWriter::open(&path).unwrap());
    drop(RawWriter::open(&extents[0]).unwrap());
    drop(busy);
    for (p, original) in std::iter::once(&path).chain(&extents).zip(&originals) {
        assert_eq!(&fs::read(p).unwrap(), original);
    }
    // An unaligned EOF cannot host the new metadata arena. The entire call
    // must refuse before modifying the allocated first extent.
    let mut bytes = fs::read(&extents[1]).unwrap();
    for at in [48, 56] {
        let directory = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize * 512;
        if directory != 0 {
            bytes[directory..directory + 4].fill(0);
        }
    }
    bytes.push(0);
    fs::write(&extents[1], bytes).unwrap();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    let before = std::iter::once(&path)
        .chain(&extents)
        .map(|p| fs::read(p).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        writer.write_all_at(65529, &[9; 23]).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        writer.write_zeroes(17, 65536).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    for (p, original) in std::iter::once(&path).chain(&extents).zip(before) {
        assert_eq!(fs::read(p).unwrap(), original);
    }
}

#[test]
fn whole_call_projects_late_alignment_and_all_appends_before_mutation() {
    use std::io::{Read, Write};
    let _boundary = process_boundary::writer_test();
    for limit_case in [false, true] {
        let (_directory, path, extents) = fixture();
        if limit_case {
            for extent in &extents {
                clear_first_mapping(extent);
            }
            let capacity = 33u64 * 1024 * 1024 * 1024;
            let remaining = capacity
                - fs::metadata(&path).unwrap().len()
                - fs::metadata(&extents[1]).unwrap().len()
                - 65536;
            fs::OpenOptions::new()
                .write(true)
                .open(&extents[0])
                .unwrap()
                .set_len(remaining / 65536 * 65536)
                .unwrap();
            let physical = fs::metadata(&path).unwrap().len()
                + extents
                    .iter()
                    .map(|extent| fs::metadata(extent).unwrap().len())
                    .sum::<u64>();
            assert!(physical + 65536 <= capacity);
            assert!(physical + 2 * 65536 > capacity);
        } else {
            clear_first_mapping(&extents[1]);
            fs::OpenOptions::new()
                .append(true)
                .open(&extents[1])
                .unwrap()
                .write_all(&[171])
                .unwrap();
        }
        let lengths = extents
            .iter()
            .map(|extent| fs::metadata(extent).unwrap().len())
            .collect::<Vec<_>>();
        let descriptor = fs::read(&path).unwrap();
        let second = fs::read(&extents[1]).unwrap();
        let mut prefix = vec![0; 131072];
        fs::File::open(&extents[0])
            .unwrap()
            .read_exact(&mut prefix)
            .unwrap();
        let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
        assert_eq!(
            writer.write_all_at(65529, &[9; 23]).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            writer.write_zeroes(65529, 23).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fs::read(&path).unwrap(), descriptor);
        assert_eq!(fs::read(&extents[1]).unwrap(), second);
        let mut after = vec![0; 131072];
        fs::File::open(&extents[0])
            .unwrap()
            .read_exact(&mut after)
            .unwrap();
        assert_eq!(after, prefix);
        for (extent, length) in extents.iter().zip(lengths) {
            assert_eq!(fs::metadata(extent).unwrap().len(), length);
            assert!(
                !extent
                    .with_file_name(format!(
                        "{}.virtdisk-transaction",
                        extent.file_name().unwrap().to_str().unwrap()
                    ))
                    .exists()
            );
        }
        assert!(
            !path
                .with_file_name("disk.vmdk.virtdisk-transaction")
                .exists()
        );
    }
}

#[test]
#[ignore = "requires independent qemu-img split sparse allocation oracle"]
fn split_sparse_allocated_holes_remain_qemu_readable() {
    let _boundary = process_boundary::subprocess_test();
    let (directory, path, extents) = fixture();
    for extent in &extents {
        clear_first_mapping(extent);
    }
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_all_at(65529, &[9; 23]).unwrap();
    writer.write_zeroes(65534, 7).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let output = directory.path().join("allocated.raw");
    let result = std::process::Command::new("qemu-img")
        .args(["convert", "-f", "vmdk", "-O", "raw"])
        .arg(&path)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut expected = vec![0; 131072];
    expected[65529..65552].fill(9);
    expected[65534..65541].fill(0);
    assert_eq!(fs::read(output).unwrap(), expected);
}

#[test]
fn late_aggregate_overflow_and_foreign_growth_preserve_first_extent_exactly() {
    use std::io::{Read, Write};
    let _boundary = process_boundary::writer_test();
    for foreign_growth in [false, true] {
        let (_directory, path, extents) = fixture();
        clear_first_mapping(&extents[1]);
        if !foreign_growth {
            let limit = 33u64 * 1024 * 1024 * 1024;
            let available = limit
                - fs::metadata(&path).unwrap().len()
                - fs::metadata(&extents[0]).unwrap().len();
            fs::OpenOptions::new()
                .write(true)
                .open(&extents[1])
                .unwrap()
                .set_len(available / 65536 * 65536)
                .unwrap();
        }
        let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
        if foreign_growth {
            fs::OpenOptions::new()
                .append(true)
                .open(&extents[1])
                .unwrap()
                .write_all(&[171])
                .unwrap();
        }
        let descriptor = fs::read(&path).unwrap();
        let first = fs::read(&extents[0]).unwrap();
        let length = fs::metadata(&extents[1]).unwrap().len();
        let mut prefix = vec![0; 131072];
        fs::File::open(&extents[1])
            .unwrap()
            .read_exact(&mut prefix)
            .unwrap();
        assert!(writer.write_all_at(65529, &[9; 23]).is_err());
        assert_eq!(fs::read(&path).unwrap(), descriptor);
        assert_eq!(fs::read(&extents[0]).unwrap(), first);
        assert_eq!(fs::metadata(&extents[1]).unwrap().len(), length);
        let mut actual = vec![0; 131072];
        fs::File::open(&extents[1])
            .unwrap()
            .read_exact(&mut actual)
            .unwrap();
        assert_eq!(actual, prefix);
        for file in std::iter::once(&path).chain(&extents) {
            assert!(
                !file
                    .with_file_name(format!(
                        "{}.virtdisk-transaction",
                        file.file_name().unwrap().to_str().unwrap()
                    ))
                    .exists()
            );
        }
    }
}

#[test]
#[ignore = "requires independent qemu-img redundant native ZERO allocation oracle"]
fn split_sparse_redundant_native_zero_allocation_preserves_masked_neighbors() {
    let _boundary = process_boundary::subprocess_test();
    let (directory, path, extents) = fixture();
    for extent in &extents {
        let mut bytes = fs::read(extent).unwrap();
        let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
        let table = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
        bytes[table..table + 4].copy_from_slice(&1u32.to_le_bytes());
        let copy = bytes[table..table + 2048].to_vec();
        let flags = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) | 6;
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        bytes[48..56].copy_from_slice(&30u64.to_le_bytes());
        bytes[30 * 512..30 * 512 + 4].copy_from_slice(&31u32.to_le_bytes());
        bytes[31 * 512..31 * 512 + 2048].copy_from_slice(&copy);
        fs::write(extent, bytes).unwrap();
    }
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_all_at(65529, &[9; 23]).unwrap();
    writer.write_zeroes(65534, 7).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let output = directory.path().join("native-zero-allocated.raw");
    let result = std::process::Command::new("qemu-img")
        .args(["convert", "-f", "vmdk", "-O", "raw"])
        .arg(&path)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut expected = vec![0; 131072];
    expected[65529..65552].fill(9);
    expected[65534..65541].fill(0);
    assert_eq!(fs::read(output).unwrap(), expected);
}

#[test]
#[ignore = "requires independent qemu-img missing table oracle"]
fn missing_table_creation_matches_qemu() {
    let _boundary = process_boundary::subprocess_test();
    let (directory, path, extents) = fixture();
    let mut bytes = fs::read(&extents[1]).unwrap();
    let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
    bytes[gd..gd + 4].fill(0);
    fs::write(&extents[1], bytes).unwrap();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_all_at(65529, &[9; 23]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let output = directory.path().join("missing.raw");
    let result = std::process::Command::new("qemu-img")
        .args(["convert", "-f", "vmdk", "-O", "raw"])
        .arg(&path)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut expected = vec![41; 65536];
    expected.extend(vec![0; 65536]);
    expected[65529..65552].fill(9);
    assert_eq!(fs::read(output).unwrap(), expected);
}

#[test]
fn existing_table_prefix_before_missing_table_preserves_payload_with_padding() {
    let _boundary = process_boundary::writer_test();
    let directory = tempfile::tempdir().unwrap();
    let extent = directory.path().join("disk-s1.vmdk");
    let writer = VmdkWriter::create(&extent, 513 * 65536).unwrap();
    writer.write_all_at(0, &vec![41; 65536]).unwrap();
    writer.write_all_at(511 * 65536, &vec![53; 65536]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let mut bytes = fs::read(&extent).unwrap();
    let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
    bytes[gd + 4..gd + 8].fill(0);
    let offset = u64::from_le_bytes(bytes[28..36].try_into().unwrap()) as usize * 512;
    let length = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize * 512;
    bytes[offset..offset + length].fill(0);
    fs::write(&extent, &bytes).unwrap();
    let path = directory.path().join("disk.vmdk");
    fs::write(&path,"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 65664 SPARSE \"disk-s1.vmdk\"\n").unwrap();
    let descriptor = fs::read(&path).unwrap();
    let boundary = 512 * 65536;
    let writer = VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).unwrap();
    writer.write_all_at(boundary - 2, &[7; 5]).unwrap();
    writer.write_zeroes(boundary - 1, 2).unwrap();
    writer.flush().unwrap();
    let mut expected = vec![0; 513 * 65536];
    expected[..65536].fill(41);
    expected[511 * 65536..boundary as usize].fill(53);
    expected[boundary as usize - 2..boundary as usize + 3].fill(7);
    expected[boundary as usize - 1..boundary as usize + 1].fill(0);
    let mut actual = vec![0; expected.len()];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, expected);
    drop(writer);
    let after = fs::read(&extent).unwrap();
    assert_eq!(&after[64..72], &bytes[64..72]);
    assert_eq!(after.len(), bytes.len() + 65536);
    assert_ne!(fs::read(&path).unwrap(), descriptor);
    let gt = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
    let final_payload =
        u32::from_le_bytes(bytes[gt + 511 * 4..gt + 512 * 4].try_into().unwrap()) as usize * 512;
    let overhead = u64::from_le_bytes(bytes[64..72].try_into().unwrap()) as usize * 512;
    assert_eq!(
        &after[overhead..final_payload + 65534],
        &bytes[overhead..final_payload + 65534]
    );
    let new_table = u32::from_le_bytes(after[gd + 4..gd + 8].try_into().unwrap()) as usize * 512;
    assert!(new_table >= 512 && new_table + 2048 <= overhead);
    assert_ne!(new_table, gt);
    assert_eq!(
        u32::from_le_bytes(after[new_table..new_table + 4].try_into().unwrap()) as usize * 512,
        bytes.len()
    );
    assert!(
        after[new_table + 4..new_table + 2048]
            .iter()
            .all(|byte| *byte == 0)
    );
    Vmdk::open_descriptor(&path, std::slice::from_ref(&extent))
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
    for _ in 0..2 {
        let writer = VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).unwrap();
        writer.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
        drop(writer);
        assert_eq!(fs::read(&extent).unwrap(), after);
    }
    for participant in [&path, &extent] {
        let mut sidecar = participant.as_os_str().to_os_string();
        sidecar.push(".virtdisk-transaction");
        assert!(!std::path::Path::new(&sidecar).exists());
    }
}

#[test]
fn missing_table_uses_padding_without_engulfing_existing_payload() {
    let _boundary = process_boundary::writer_test();
    padding_fixture_check(false);
}
#[test]
#[ignore = "requires independent qemu-img allocated-extent padding oracle"]
fn missing_padding_table_matches_qemu() {
    let _boundary = process_boundary::subprocess_test();
    padding_fixture_check(true);
}
fn padding_fixture_check(native: bool) {
    let (directory, path, extents) = fixture();
    let mut bytes = fs::read(&extents[1]).unwrap();
    bytes[12..20].copy_from_slice(&65664u64.to_le_bytes());
    let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
    bytes[gd + 4..gd + 8].fill(0);
    bytes[13312..15360].fill(83);
    fs::write(&extents[1], &bytes).unwrap();
    let descriptor = fs::read_to_string(&path).unwrap().replace(
        "RW 128 SPARSE \"disk-s2.vmdk\"",
        "RW 65664 SPARSE \"disk-s2.vmdk\"",
    );
    fs::write(&path, descriptor).unwrap();
    let eof = bytes.len() as u64;
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer
        .write_all_at(65536 + 512 * 65536 + 17, &[7; 19])
        .unwrap();
    writer.flush().unwrap();
    drop(writer);
    let after = fs::read(&extents[1]).unwrap();
    assert_eq!(&after[64..72], &bytes[64..72]);
    assert_eq!(&after[65536..131072], &bytes[65536..131072]);
    assert_eq!(after.len() as u64, eof + 65536);
    assert_eq!(
        u32::from_le_bytes(after[gd + 4..gd + 8].try_into().unwrap()),
        26
    );
    let reader = Vmdk::open_descriptor(&path, &extents).unwrap();
    let mut old = [0; 65536];
    reader.read_exact_at(65536, &mut old).unwrap();
    assert_eq!(old, [42; 65536]);
    let mut last = [0; 65536];
    reader
        .read_exact_at(65536 + 512 * 65536, &mut last)
        .unwrap();
    let mut expected = [0; 65536];
    expected[17..36].fill(7);
    assert_eq!(last, expected);
    if native {
        let output = directory.path().join("padding.raw");
        let result = std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vmdk", "-O", "raw"])
            .arg(&path)
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut expected = vec![0; 514 * 65536];
        expected[..65536].fill(41);
        expected[65536..131072].fill(42);
        expected[513 * 65536 + 17..513 * 65536 + 36].fill(7);
        assert_eq!(fs::read(output).unwrap(), expected);
    }
}

#[test]
fn padding_table_cache_reserves_newly_claimed_ranges_and_refuses_no_space() {
    let _boundary = process_boundary::writer_test();
    for no_space in [false, true] {
        let (_directory, path, extents) = fixture();
        let first_before = fs::read(&extents[0]).unwrap();
        let mut bytes = fs::read(&extents[1]).unwrap();
        bytes[12..20].copy_from_slice(&131200u64.to_le_bytes());
        let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
        bytes[gd + 4..gd + 12].fill(0);
        if no_space {
            bytes[64..72].copy_from_slice(&26u64.to_le_bytes());
        }
        fs::write(&extents[1], &bytes).unwrap();
        let descriptor = fs::read_to_string(&path).unwrap().replace(
            "RW 128 SPARSE \"disk-s2.vmdk\"",
            "RW 131200 SPARSE \"disk-s2.vmdk\"",
        );
        fs::write(&path, &descriptor).unwrap();
        let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
        if no_space {
            assert_eq!(
                writer
                    .write_all_at(65529, &vec![9; 512 * 65536 + 24])
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported
            );
            assert_eq!(
                writer
                    .write_zeroes(65529, 512 * 65536 + 24)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported
            );
            assert_eq!(fs::read(&extents[0]).unwrap(), first_before);
            for participant in std::iter::once(&path).chain(&extents) {
                let mut sidecar = participant.as_os_str().to_os_string();
                sidecar.push(".virtdisk-transaction");
                assert!(!std::path::Path::new(&sidecar).exists());
            }
            assert_eq!(fs::read(&extents[1]).unwrap(), bytes);
            assert_eq!(fs::read_to_string(&path).unwrap(), descriptor);
            continue;
        }
        writer.write_all_at(65536 + 512 * 65536, &[7; 19]).unwrap();
        writer.write_all_at(65536 + 1024 * 65536, &[8; 19]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let after = fs::read(&extents[1]).unwrap();
        assert_eq!(
            u32::from_le_bytes(after[gd + 4..gd + 8].try_into().unwrap()),
            26
        );
        assert_eq!(
            u32::from_le_bytes(after[gd + 8..gd + 12].try_into().unwrap()),
            30
        );
        assert_eq!(after.len(), bytes.len() + 131072);
        let reader = Vmdk::open_descriptor(&path, &extents).unwrap();
        let mut first = [0; 19];
        let mut last = [0; 19];
        reader
            .read_exact_at(65536 + 512 * 65536, &mut first)
            .unwrap();
        reader
            .read_exact_at(65536 + 1024 * 65536, &mut last)
            .unwrap();
        assert_eq!(first, [7; 19]);
        assert_eq!(last, [8; 19]);
    }
}

#[test]
fn padding_search_preserves_entire_directory_sector() {
    let _boundary = process_boundary::writer_test();
    let (_directory, path, extents) = fixture();
    let mut bytes = fs::read(&extents[1]).unwrap();
    bytes[12..20].copy_from_slice(&65664u64.to_le_bytes());
    let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
    let old_table = bytes[11264..13312].to_vec();
    bytes[40 * 512..44 * 512].copy_from_slice(&old_table);
    bytes[gd..gd + 4].copy_from_slice(&40u32.to_le_bytes());
    bytes[gd + 4..gd + 8].fill(0);
    bytes[gd + 8..gd + 512].fill(83);
    fs::write(&extents[1], &bytes).unwrap();
    let descriptor = fs::read_to_string(&path).unwrap().replace(
        "RW 128 SPARSE \"disk-s2.vmdk\"",
        "RW 65664 SPARSE \"disk-s2.vmdk\"",
    );
    fs::write(&path, descriptor).unwrap();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_all_at(65536 + 512 * 65536, &[7; 19]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let after = fs::read(&extents[1]).unwrap();
    assert_eq!(
        u32::from_le_bytes(after[gd + 4..gd + 8].try_into().unwrap()),
        22
    );
    assert_eq!(&after[gd + 8..gd + 512], &bytes[gd + 8..gd + 512]);
}

#[test]
fn multiple_missing_tables_share_complete_call_plan() {
    let _boundary = process_boundary::writer_test();
    multiple_missing_check(true, false);
}
#[test]
fn multiple_missing_empty_tables_share_arena() {
    let _boundary = process_boundary::writer_test();
    multiple_missing_check(false, false);
}
#[test]
fn existing_empty_prefix_is_above_prepared_metadata() {
    let _boundary = process_boundary::writer_test();
    multiple_missing_check(false, true);
}
fn multiple_missing_check(allocated: bool, prefix: bool) {
    {
        let (_directory, path, extents) = fixture();
        let mut bytes = fs::read(&extents[1]).unwrap();
        let grains = if allocated { 1025 } else { 513 };
        bytes[12..20].copy_from_slice(&(grains * 128u64).to_le_bytes());
        let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
        if !allocated {
            let gt = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
            bytes[gt..gt + 2048].fill(0);
        }
        let first_missing = usize::from(allocated || prefix);
        bytes[gd + first_missing * 4..gd + (grains as usize).div_ceil(512) * 4].fill(0);
        let eof = bytes.len();
        let overhead = bytes[64..72].to_vec();
        fs::write(&extents[1], &bytes).unwrap();
        let descriptor = fs::read_to_string(&path).unwrap().replace(
            "RW 128 SPARSE \"disk-s2.vmdk\"",
            &format!("RW {} SPARSE \"disk-s2.vmdk\"", grains * 128),
        );
        fs::write(&path, descriptor).unwrap();
        let boundary = if allocated {
            1024 * 65536u64
        } else {
            512 * 65536
        };
        let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
        writer.write_all_at(65536 + boundary - 1, &[7; 2]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let after = fs::read(&extents[1]).unwrap();
        assert_eq!(
            after.len(),
            eof + 131072 + if allocated { 0 } else { 65536 }
        );
        if allocated {
            assert_eq!(&after[64..72], &overhead);
        } else {
            assert_eq!(
                u64::from_le_bytes(after[64..72].try_into().unwrap()) * 512,
                (eof + 65536) as u64
            );
        }
        let reader = Vmdk::open_descriptor(&path, &extents).unwrap();
        let mut actual = [0; 4];
        reader
            .read_exact_at(65536 + boundary - 2, &mut actual)
            .unwrap();
        assert_eq!(actual, [0, 7, 7, 0]);
        let mut first = [0; 65536];
        reader.read_exact_at(65536, &mut first).unwrap();
        assert_eq!(first, [if allocated { 42 } else { 0 }; 65536]);
        VmdkWriter::open_descriptor(&path, &extents)
            .unwrap()
            .read_exact_at(65536 + boundary - 2, &mut actual)
            .unwrap();
        assert_eq!(actual, [0, 7, 7, 0]);
    }
}

#[test]
fn multiple_table_plan_reserves_late_hole_before_any_mutation() {
    let _boundary = process_boundary::writer_test();
    let (_directory, path, extents) = fixture();
    let first = fs::read(&extents[0]).unwrap();
    let mut bytes = fs::read(&extents[1]).unwrap();
    bytes[12..20].copy_from_slice(&131200u64.to_le_bytes());
    let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
    bytes[gd + 4..gd + 12].fill(0);
    bytes[64..72].copy_from_slice(&30u64.to_le_bytes());
    fs::write(&extents[1], &bytes).unwrap();
    let descriptor = fs::read_to_string(&path).unwrap().replace(
        "RW 128 SPARSE \"disk-s2.vmdk\"",
        "RW 131200 SPARSE \"disk-s2.vmdk\"",
    );
    fs::write(&path, &descriptor).unwrap();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    let boundary = 65536 + 1024 * 65536;
    assert_eq!(
        writer
            .write_all_at(boundary - 1, &[7; 2])
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        writer.write_zeroes(boundary - 1, 2).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fs::read(&path).unwrap(), descriptor.as_bytes());
    assert_eq!(fs::read(&extents[0]).unwrap(), first);
    assert_eq!(fs::read(&extents[1]).unwrap(), bytes);
    for participant in std::iter::once(&path).chain(&extents) {
        let mut sidecar = participant.as_os_str().to_os_string();
        sidecar.push(".virtdisk-transaction");
        assert!(!std::path::Path::new(&sidecar).exists());
    }
    writer.write_all_at(boundary - 1, &[7]).unwrap();
}
#[test]
fn later_table_reuses_prepared_arena_and_zero_chunks_prepare_once() {
    let _boundary = process_boundary::writer_test();
    let (_directory, path, extents) = fixture();
    let mut bytes = vec![0; 131072];
    let original = fs::read(&extents[1]).unwrap();
    bytes[..512].copy_from_slice(&original[..512]);
    bytes[12..20].copy_from_slice(&131200u64.to_le_bytes());
    bytes[28..36].copy_from_slice(&6u64.to_le_bytes());
    bytes[36..44].copy_from_slice(&250u64.to_le_bytes());
    bytes[56..64].copy_from_slice(&1u64.to_le_bytes());
    bytes[64..72].copy_from_slice(&256u64.to_le_bytes());
    bytes[512..516].copy_from_slice(&2u32.to_le_bytes());
    fs::write(&extents[1], &bytes).unwrap();
    let descriptor = fs::read_to_string(&path).unwrap().replace(
        "RW 128 SPARSE \"disk-s2.vmdk\"",
        "RW 131200 SPARSE \"disk-s2.vmdk\"",
    );
    fs::write(&path, descriptor).unwrap();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_zeroes(65536 + 512 * 65536 - 1, 65538).unwrap();
    let after_first = fs::metadata(&extents[1]).unwrap().len();
    assert_eq!(after_first, 131072 + 65536 + 3 * 65536);
    writer.write_all_at(65536 + 1024 * 65536, &[7; 19]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let after = fs::read(&extents[1]).unwrap();
    let overhead = u64::from_le_bytes(after[64..72].try_into().unwrap()) * 512;
    assert_eq!(overhead, 196608);
    let gt2 = u32::from_le_bytes(after[520..524].try_into().unwrap()) as u64 * 512;
    assert!(gt2 >= 131072 && gt2 + 2048 <= overhead);
    assert_eq!(after.len() as u64, after_first + 65536);
    let reader = Vmdk::open_descriptor(&path, &extents).unwrap();
    let mut actual = [0; 19];
    reader
        .read_exact_at(65536 + 1024 * 65536, &mut actual)
        .unwrap();
    assert_eq!(actual, [7; 19]);
}
