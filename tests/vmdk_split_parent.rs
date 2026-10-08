#![cfg(feature = "std")]
//! Backed split sparse writes retain authorized immutable parent graphs.
#![cfg(target_os = "linux")]
use std::{fs, path::PathBuf};
use virtdisk::{ReadAt, Vmdk, VmdkWriter};

#[test]
fn common_writer_dispatches_backed_split_cow_and_retains_parent_locks() {
    use virtdisk::{ImageFormat, ImageOperation, ImageWriter, InspectImage, WriteAt};
    let f = fixture();
    let before = snapshot(&f.parent);
    let mut authorized = f.child_extents.clone();
    authorized.extend(f.parent.clone());
    let writer = ImageWriter::open_chain(&f.child, ImageFormat::Vmdk, &authorized).unwrap();
    assert!(writer.inspection().has_parent);
    assert_eq!(
        writer.inspection().capabilities.get(ImageOperation::Write),
        virtdisk::Capability::Supported
    );
    for path in &f.parent {
        assert_eq!(
            virtdisk::RawWriter::open(path).err().unwrap().kind(),
            virtdisk::io::ErrorKind::WouldBlock
        );
    }
    let mut actual = vec![0; f.expected.len()];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, f.expected);
    writer.write_all_at(65530, &[7; 20]).unwrap();
    writer.flush().unwrap();
    let mut expected = f.expected;
    expected[65530..65550].fill(7);
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, expected);
    drop(writer);
    assert_eq!(snapshot(&f.parent), before);
    let writer = ImageWriter::open_chain(&f.child, ImageFormat::Vmdk, &authorized).unwrap();
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, expected);
}

struct Fixture {
    _directory: tempfile::TempDir,
    parent: Vec<PathBuf>,
    child: PathBuf,
    child_extents: Vec<PathBuf>,
    sibling: PathBuf,
    sibling_extents: Vec<PathBuf>,
    expected: Vec<u8>,
}
fn split_descriptor(
    directory: &std::path::Path,
    name: &str,
    parent: Option<(&str, &str)>,
    allocated: bool,
) -> (PathBuf, Vec<PathBuf>) {
    let path = directory.join(format!("{name}.vmdk"));
    let mut extents = Vec::new();
    for (index, size) in [65536, 66048].into_iter().enumerate() {
        let extent = directory.join(format!("{name}-s{}.vmdk", index + 1));
        let writer = VmdkWriter::create_sparse(&extent, size).unwrap();
        if allocated {
            writer
                .write_all_at(0, &vec![31 + index as u8; size as usize])
                .unwrap();
            writer.flush().unwrap();
        }
        drop(writer);
        let mut bytes = fs::read(&extent).unwrap();
        let offset = u64::from_le_bytes(bytes[28..36].try_into().unwrap()) as usize * 512;
        let length = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize * 512;
        bytes[offset..offset + length].fill(0);
        fs::write(&extent, bytes).unwrap();
        extents.push(extent);
    }
    let (parent_cid, hint) = parent.map_or(("ffffffff", String::new()), |(cid, hint)| {
        (cid, format!("parentFileNameHint=\"{hint}\"\n"))
    });
    fs::write(&path, format!("version=1\nCID=12345678\nparentCID={parent_cid}\n{hint}createType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"{name}-s1.vmdk\"\nRW 129 SPARSE \"{name}-s2.vmdk\"\n")).unwrap();
    (path, extents)
}
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let (parent, parent_extents) = split_descriptor(directory.path(), "parent", None, true);
    let mut parent_files = vec![parent];
    parent_files.extend(parent_extents);
    let (child, child_extents) = split_descriptor(
        directory.path(),
        "child",
        Some(("12345678", "parent.vmdk")),
        false,
    );
    let (sibling, sibling_extents) = split_descriptor(
        directory.path(),
        "sibling",
        Some(("12345678", "parent.vmdk")),
        false,
    );
    let mut expected = vec![31; 65536];
    expected.extend(vec![32; 66048]);
    Fixture {
        _directory: directory,
        parent: parent_files,
        child,
        child_extents,
        sibling,
        sibling_extents,
        expected,
    }
}
fn snapshot(paths: &[PathBuf]) -> Vec<Vec<u8>> {
    paths.iter().map(|path| fs::read(path).unwrap()).collect()
}
fn directory_entries(path: &std::path::Path) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[test]
fn authorized_split_child_cows_across_extents_and_final_partial_grain() {
    let f = fixture();
    let parent_before = snapshot(&f.parent);
    let mut child_files = vec![f.child.clone()];
    child_files.extend(f.child_extents.clone());
    let child_before = snapshot(&child_files);
    let entries_before = directory_entries(f._directory.path());
    let mut authorized = f.child_extents.clone();
    assert!(VmdkWriter::open_chain(&f.child, &authorized).is_err());
    assert_eq!(snapshot(&f.parent), parent_before);
    assert_eq!(snapshot(&child_files), child_before);
    assert_eq!(
        directory_entries(f._directory.path()),
        entries_before,
        "denied parent must not publish a sidecar or temporary file"
    );
    authorized.extend(f.parent.clone());
    let reader = Vmdk::open_chain(&f.child, &authorized).unwrap();
    let mut inherited = vec![0; f.expected.len()];
    reader.read_exact_at(0, &mut inherited).unwrap();
    assert_eq!(
        inherited, f.expected,
        "reader establishes a valid inherited fixture"
    );
    drop(reader);
    let writer = VmdkWriter::open_chain(&f.child, &authorized).unwrap();
    assert!(writer.has_parent());
    for parent in &f.parent {
        assert_eq!(
            virtdisk::RawWriter::open(parent).err().unwrap().kind(),
            virtdisk::io::ErrorKind::WouldBlock,
            "retained parent lock must exclude cooperating writers"
        );
    }
    writer.read_exact_at(0, &mut inherited).unwrap();
    assert_eq!(inherited, f.expected);
    writer.write_all_at(65530, &[7; 20]).unwrap();
    writer
        .write_all_at(f.expected.len() as u64 - 1, &[9])
        .unwrap();
    let mut expected = f.expected.clone();
    expected[65530..65550].fill(7);
    *expected.last_mut().unwrap() = 9;
    writer.read_exact_at(0, &mut inherited).unwrap();
    assert_eq!(
        inherited, expected,
        "private grains preserve inherited surroundings"
    );
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(snapshot(&f.parent), parent_before);
    let reopened = Vmdk::open_chain(&f.child, &authorized).unwrap();
    reopened.read_exact_at(0, &mut inherited).unwrap();
    assert_eq!(inherited, expected);
    let mut sibling_authorized = f.sibling_extents.clone();
    sibling_authorized.extend(f.parent);
    let sibling = Vmdk::open_chain(&f.sibling, &sibling_authorized).unwrap();
    sibling.read_exact_at(0, &mut inherited).unwrap();
    assert_eq!(inherited, f.expected);
}

#[test]
fn child_zero_mask_does_not_copy_parent_bytes_during_partial_allocation() {
    let f = fixture();
    let parent_before = snapshot(&f.parent);
    let mask_source = f._directory.path().join("zero-mask.vmdk");
    drop(VmdkWriter::create(&mask_source, 65536).unwrap());
    let mut bytes = fs::read(&mask_source).unwrap();
    let descriptor_offset = u64::from_le_bytes(bytes[28..36].try_into().unwrap()) as usize * 512;
    let descriptor_length = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize * 512;
    bytes[descriptor_offset..descriptor_offset + descriptor_length].fill(0);
    let flags = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) | 4;
    bytes[8..12].copy_from_slice(&flags.to_le_bytes());
    for field in [48, 56] {
        let directory =
            u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap()) as usize * 512;
        if directory == 0 {
            continue;
        }
        let table =
            u32::from_le_bytes(bytes[directory..directory + 4].try_into().unwrap()) as usize * 512;
        bytes[table..table + 4].copy_from_slice(&1u32.to_le_bytes());
    }
    fs::write(&f.child_extents[0], bytes).unwrap();
    let mut authorized = f.child_extents.clone();
    authorized.extend(f.parent.clone());
    let reader = Vmdk::open_chain(&f.child, &authorized).unwrap();
    let mut actual = vec![255; 65536];
    reader.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(
        actual,
        vec![0; 65536],
        "reader confirms a valid ZERO fixture"
    );
    drop(reader);
    let writer = VmdkWriter::open_chain(&f.child, &authorized).unwrap();
    writer.write_all_at(31, &[9; 3]).unwrap();
    writer.write_zeroes(65536 + 19, 17).unwrap();
    writer.read_exact_at(0, &mut actual).unwrap();
    let mut expected = vec![0; 65536];
    expected[31..34].fill(9);
    assert_eq!(actual, expected);
    let mut inherited = vec![0; 65536];
    writer.read_exact_at(65536, &mut inherited).unwrap();
    let mut expected_inherited = vec![32; 65536];
    expected_inherited[19..36].fill(0);
    assert_eq!(
        inherited, expected_inherited,
        "partial zeroing preserves inherited surroundings"
    );
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(snapshot(&f.parent), parent_before);
    let reopened = Vmdk::open_chain(&f.child, &authorized).unwrap();
    reopened.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, expected);
}
