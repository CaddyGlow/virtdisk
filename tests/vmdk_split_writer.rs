use std::{fs, io, process::Command};
use virtdisk::VmdkWriter;
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn descriptor() -> &'static str {
    "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\nRW 1 FLAT \"first.vmdk\" 1\nRW 2 FLAT \"second.vmdk\" 2\n"
}
#[test]
fn split_authorization_locks_and_cross_boundary_io_preserve_extent_neighbors() {
    let _serial = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vmdk");
    let first = dir.path().join("first.vmdk");
    let second = dir.path().join("second.vmdk");
    fs::write(&path, descriptor()).unwrap();
    fs::write(&first, vec![18; 1536]).unwrap();
    fs::write(&second, vec![29; 2560]).unwrap();
    assert!(VmdkWriter::open_descriptor(&path, std::slice::from_ref(&first)).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), descriptor());
    // Failure acquiring a later extent must release every earlier retained lock.
    let busy = virtdisk::RawWriter::open(&second).unwrap();
    assert!(VmdkWriter::open_descriptor(&path, &[first.clone(), second.clone()]).is_err());
    virtdisk::RawWriter::open(&path).unwrap();
    virtdisk::RawWriter::open(&first).unwrap();
    drop(busy);
    let writer = VmdkWriter::open_descriptor(&path, &[first.clone(), second.clone()]).unwrap();
    assert_eq!(writer.len(), 1536);
    for locked in [&path, &first, &second] {
        assert_eq!(
            virtdisk::RawWriter::open(locked).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    let mut out = [0; 24];
    writer.read_exact_at(500, &mut out).unwrap();
    assert_eq!(&out[..12], &[18; 12]);
    assert_eq!(&out[12..], &[29; 12]);
    writer.write_all_at(505, &[83; 19]).unwrap();
    writer.write_zeroes(509, 7).unwrap();
    writer.flush().unwrap();
    let mut a = vec![18; 1536];
    a[1017..1024].fill(83);
    a[1021..1024].fill(0);
    let mut b = vec![29; 2560];
    b[1024..1036].fill(83);
    b[1024..1028].fill(0);
    assert_eq!(fs::read(&first).unwrap(), a);
    assert_eq!(fs::read(&second).unwrap(), b);
    assert_ne!(fs::read_to_string(&path).unwrap(), descriptor());
    assert!(writer.write_zeroes(1535, 2).is_err());
    drop(writer);
    let writer = VmdkWriter::open_descriptor(&path, &[first, second]).unwrap();
    writer.read_exact_at(500, &mut out).unwrap();
    assert_eq!(&out[..5], &[18; 5]);
    assert_eq!(&out[5..9], &[83; 4]);
    assert_eq!(&out[9..16], &[0; 7]);
    assert_eq!(&out[16..], &[83; 8]);
}
#[test]
fn repeated_paths_hardlinks_zero_lengths_and_wrong_profiles_are_rejected() {
    let _serial = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vmdk");
    let first = dir.path().join("first.vmdk");
    let second = dir.path().join("second.vmdk");
    fs::write(&first, vec![18; 4096]).unwrap();
    fs::write(&second, vec![29; 4096]).unwrap();
    for text in [
        descriptor().replace("second.vmdk", "first.vmdk"),
        descriptor().replace("RW 1", "RW 0"),
        descriptor().replace("twoGbMaxExtentFlat", "monolithicFlat"),
        descriptor().replace("RW 2 FLAT", "RW 2 SPARSE"),
    ] {
        fs::write(&path, &text).unwrap();
        assert!(VmdkWriter::open_descriptor(&path, &[first.clone(), second.clone()]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
    fs::write(&path, descriptor()).unwrap();
    fs::remove_file(&second).unwrap();
    fs::hard_link(&first, &second).unwrap();
    assert!(VmdkWriter::open_descriptor(&path, &[first, second]).is_err());
}
#[test]
#[ignore = "requires independent qemu-img split-flat oracle"]
fn qemu_split_flat_fixture_cross_boundary_writes_convert_to_exact_raw_ranges() {
    use std::io::{Read, Seek, SeekFrom};
    let _serial = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vmdk");
    let output = dir.path().join("after.raw");
    assert!(
        Command::new("qemu-img")
            .args(["create", "-f", "vmdk", "-o", "subformat=twoGbMaxExtentFlat"])
            .arg(&path)
            .arg("2147484160")
            .status()
            .unwrap()
            .success()
    );
    let text = fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("RW "))
        .collect();
    assert!(lines.len() > 1);
    let boundary = lines[0]
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u64>()
        .unwrap()
        * 512;
    let extents: Vec<_> = lines
        .iter()
        .map(|line| dir.path().join(line.split('"').nth(1).unwrap()))
        .collect();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    writer.write_all_at(boundary - 7, &[96; 23]).unwrap();
    writer.write_zeroes(boundary - 2, 9).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert!(
        Command::new("qemu-img")
            .args(["convert", "-f", "vmdk", "-O", "raw"])
            .arg(&path)
            .arg(&output)
            .status()
            .unwrap()
            .success()
    );
    let mut file = fs::File::open(output).unwrap();
    file.seek(SeekFrom::Start(boundary - 16)).unwrap();
    let mut actual = [0; 64];
    file.read_exact(&mut actual).unwrap();
    let mut expected = [0; 64];
    expected[9..32].fill(96);
    expected[14..23].fill(0);
    assert_eq!(actual, expected);
    assert_eq!(file.metadata().unwrap().len(), 2147484160);
}
