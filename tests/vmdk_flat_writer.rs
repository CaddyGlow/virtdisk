#![cfg(feature = "std")]
use std::{fs, process::Command};
use virtdisk::VmdkWriter;
use virtdisk::io;
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn descriptor() -> &'static str {
    "# Disk DescriptorFile\nversion=1\nCID=C\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\nRW 2 FLAT \"extent-flat.vmdk\" 1\n"
}
#[test]
fn authorized_flat_writer_locks_both_files_changes_cid_and_respects_extent_slice() {
    let _serial = SERIAL.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let extent = directory.path().join("extent-flat.vmdk");
    fs::write(&path, descriptor()).unwrap();
    fs::write(&extent, vec![31; 2048]).unwrap();
    let original = fs::read(&path).unwrap();
    assert!(VmdkWriter::open_descriptor(&path, &[]).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    let writer = VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).unwrap();
    assert!(writer.is_descriptor());
    assert!(!writer.has_parent());
    assert_eq!(writer.len(), 1024);
    for locked in [&path, &extent] {
        assert_eq!(
            virtdisk::RawWriter::open(locked).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    writer.write_all_at(0, &[]).unwrap();
    writer.write_zeroes(0, 0).unwrap();
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(writer.write_all_at(1023, &[1, 2]).is_err());
    assert!(writer.write_zeroes(1024, 1).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    writer.write_all_at(507, &[81; 17]).unwrap();
    writer.write_zeroes(19, 11).unwrap();
    writer.flush().unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("CID="));
    assert_ne!(text, descriptor());
    let cid = text
        .lines()
        .find_map(|line| line.strip_prefix("CID="))
        .unwrap();
    assert_ne!(u32::from_str_radix(cid, 16).unwrap(), 12);
    let mut expected = vec![31; 2048];
    expected[512 + 507..512 + 524].fill(81);
    expected[512 + 19..512 + 30].fill(0);
    assert_eq!(fs::read(&extent).unwrap(), expected);
    let mut logical = vec![0; 1024];
    writer.read_exact_at(0, &mut logical).unwrap();
    assert_eq!(logical, expected[512..1536]);
    drop(writer);
    let writer = VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).unwrap();
    writer.read_exact_at(0, &mut logical).unwrap();
    assert_eq!(logical, expected[512..1536]);
}
#[test]
fn malformed_profiles_aliases_and_pending_files_fail_before_mutation() {
    let _serial = SERIAL.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let extent = directory.path().join("extent-flat.vmdk");
    fs::write(&extent, vec![43; 2048]).unwrap();
    for text in [
        descriptor().replace("monolithicFlat", "streamOptimized"),
        descriptor().replace("parentCID=ffffffff", "parentCID=00000001"),
        format!("{}CID=1\n", descriptor()),
        format!("{}RW 2 FLAT \"extent-flat.vmdk\" 1\n", descriptor()),
        descriptor().replace("RW 2", "RW 9"),
        descriptor().replace(" 1\n", " 18446744073709551615\n"),
        descriptor().replace("extent-flat.vmdk", "file://extent-flat.vmdk"),
    ] {
        fs::write(&path, &text).unwrap();
        assert!(VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(fs::read(&extent).unwrap(), vec![43; 2048]);
    }
    fs::write(&path, descriptor()).unwrap();
    let pending = extent.with_file_name("extent-flat.vmdk.virtdisk-transaction");
    fs::write(&pending, b"pending").unwrap();
    assert!(VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).is_err());
    fs::remove_file(pending).unwrap();
    fs::remove_file(&extent).unwrap();
    fs::hard_link(&path, &extent).unwrap();
    assert!(VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).is_err());
}
#[test]
#[ignore = "requires independent qemu-img flat VMDK oracle"]
fn qemu_flat_fixture_remains_native_readable_after_writes() {
    let _serial = SERIAL.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source.raw");
    let path = directory.path().join("disk.vmdk");
    let extent = directory.path().join("disk-flat.vmdk");
    let output = directory.path().join("after.raw");
    let mut expected = vec![59; 131072];
    fs::write(&raw, &expected).unwrap();
    assert!(
        Command::new("qemu-img")
            .args([
                "convert",
                "-f",
                "raw",
                "-O",
                "vmdk",
                "-o",
                "subformat=monolithicFlat"
            ])
            .arg(&raw)
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let writer = VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).unwrap();
    writer.write_all_at(65529, &[6; 23]).unwrap();
    expected[65529..65552].fill(6);
    writer.write_zeroes(131040, 19).unwrap();
    expected[131040..131059].fill(0);
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
    assert_eq!(fs::read(output).unwrap(), expected);
}

#[test]
fn hosted_uppercase_short_cid_updates_value_without_corrupting_property_name() {
    let _serial = SERIAL.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hosted.vmdk");
    drop(VmdkWriter::create(&path, 65536).unwrap());
    let mut bytes = fs::read(&path).unwrap();
    let end = bytes[512..10752].iter().position(|b| *b == 0).unwrap() + 512;
    let text = std::str::from_utf8(&bytes[512..end]).unwrap();
    let old = text.lines().find(|line| line.starts_with("CID=")).unwrap();
    let text = text.replacen(old, "CID=C", 1);
    bytes[512..10752].fill(0);
    bytes[512..512 + text.len()].copy_from_slice(text.as_bytes());
    fs::write(&path, bytes).unwrap();
    let writer = VmdkWriter::open(&path).unwrap();
    writer.write_all_at(0, &[8]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let bytes = fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&bytes[512..10752]);
    let cid = text
        .lines()
        .find_map(|line| line.strip_prefix("CID="))
        .expect("CID property name must survive update");
    assert_ne!(u32::from_str_radix(cid, 16).unwrap(), 12);
    VmdkWriter::open(&path).unwrap();
}

#[test]
fn factory_flat_profile_reports_allocation_geometry_and_retains_extent_lock() {
    use virtdisk::{ImageFormat, ImageProfile, ImageWriter, InspectImage, WriteAt};
    let _serial = SERIAL.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let extent = directory.path().join("extent-flat.vmdk");
    fs::write(&path, descriptor()).unwrap();
    fs::write(&extent, vec![22; 2048]).unwrap();
    assert!(ImageWriter::open(&path, ImageFormat::Vmdk).is_err());
    assert!(ImageWriter::open_chain(&path, ImageFormat::Vmdk, &[]).is_err());
    let writer =
        ImageWriter::open_chain(&path, ImageFormat::Vmdk, std::slice::from_ref(&extent)).unwrap();
    let report = writer.inspection();
    assert_eq!(report.profile, ImageProfile::Vmdk { descriptor: true });
    assert_eq!(report.geometry.allocation_block_size, None);
    assert_eq!(report.geometry.virtual_size, 1024);
    assert!(!report.has_parent);
    for locked in [&path, &extent] {
        assert_eq!(
            virtdisk::RawWriter::open(locked).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    writer.write_all_at(500, &[77; 19]).unwrap();
    writer.write_zeroes(1010, 8).unwrap();
    writer.flush().unwrap();
    let mut out = [0; 27];
    writer.read_exact_at(496, &mut out).unwrap();
    assert_eq!(&out[..4], &[22; 4]);
    assert_eq!(&out[4..23], &[77; 19]);
    assert_eq!(&out[23..], &[22; 4]);
    drop(writer);
    let bytes = fs::read(&extent).unwrap();
    assert_eq!(&bytes[..512], &[22; 512]);
    assert_eq!(&bytes[1536..], &[22; 512]);
    assert_eq!(&bytes[1522..1530], &[0; 8]);
}

#[test]
fn busy_extent_failed_open_releases_descriptor_and_parallel_writes_serialize() {
    let _serial = SERIAL.lock().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let extent = directory.path().join("extent-flat.vmdk");
    fs::write(&path, descriptor()).unwrap();
    fs::write(&extent, vec![3; 2048]).unwrap();
    let lock = virtdisk::RawWriter::open(&extent).unwrap();
    assert!(VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).is_err());
    virtdisk::RawWriter::open(&path).unwrap();
    drop(lock);
    let writer = std::sync::Arc::new(
        VmdkWriter::open_descriptor(&path, std::slice::from_ref(&extent)).unwrap(),
    );
    let first = writer.clone();
    let second = writer.clone();
    let a = std::thread::spawn(move || first.write_all_at(0, &[11; 512]).unwrap());
    let b = std::thread::spawn(move || second.write_all_at(512, &[12; 512]).unwrap());
    a.join().unwrap();
    b.join().unwrap();
    writer.flush().unwrap();
    let mut out = [0; 1024];
    writer.read_exact_at(0, &mut out).unwrap();
    assert_eq!(&out[..512], &[11; 512]);
    assert_eq!(&out[512..], &[12; 512]);
}
