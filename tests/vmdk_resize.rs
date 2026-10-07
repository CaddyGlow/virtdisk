#![cfg(target_os = "linux")]
use std::{fs, io, sync::Arc};
use virtdisk::{ReadAt, ShrinkPolicy, Vmdk, VmdkWriter};

#[test]
fn native_resize_preserves_data_grows_tables_and_zeroes_regrown_tail() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let mut writer = VmdkWriter::create_sparse(&path, 65536 + 512).unwrap();
    writer.write_all_at(65530, &[41; 32]).unwrap();
    writer.resize(33554432 + 512, ShrinkPolicy::Reject).unwrap();
    assert!(VmdkWriter::open(&path).is_err());
    let mut bytes = [0; 32];
    writer.read_exact_at(65530, &mut bytes).unwrap();
    assert_eq!(bytes, [41; 32]);
    writer.write_all_at(33554432, &[73; 512]).unwrap();
    let before = fs::read(&path).unwrap();
    assert_eq!(
        writer
            .resize(65536, ShrinkPolicy::RequireZero)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    writer.resize(65536, ShrinkPolicy::AllowDataLoss).unwrap();
    writer.resize(33554432 + 512, ShrinkPolicy::Reject).unwrap();
    let mut bytes = vec![1; 33554432 + 512];
    writer.read_exact_at(0, &mut bytes).unwrap();
    let mut expected = vec![0; bytes.len()];
    expected[65530..65536].fill(41);
    assert_eq!(bytes, expected);
    writer.flush().unwrap();
    drop(writer);
    let reader = Vmdk::open(Arc::new(virtdisk::RawDisk::open(&path).unwrap())).unwrap();
    assert_eq!(reader.len(), expected.len() as u64);
    reader.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, expected);
}

#[test]
fn backed_resize_is_unsupported_before_cid_or_payload_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent.vmdk");
    drop(VmdkWriter::create_sparse(&parent, 131072).unwrap());
    let path = directory.path().join("child.vmdk");
    let mut writer =
        VmdkWriter::create_overlay(&path, &parent, std::slice::from_ref(&parent)).unwrap();
    let before = fs::read(&path).unwrap();
    assert_eq!(
        writer
            .resize(262144, ShrinkPolicy::Reject)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn sparse_growth_reaches_maximum_capacity_and_shrink_clears_zero_grain_entries() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    drop(VmdkWriter::create_sparse(&path, 131072).unwrap());
    let mut bytes = fs::read(&path).unwrap();
    bytes[8..12].copy_from_slice(&5u32.to_le_bytes());
    let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
    let table = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
    bytes[table + 4..table + 8].copy_from_slice(&1u32.to_le_bytes());
    fs::write(&path, bytes).unwrap();
    let mut writer = VmdkWriter::open(&path).unwrap();
    writer.resize(512, ShrinkPolicy::RequireZero).unwrap();
    let maximum = 32 * 1024 * 1024 * 1024;
    writer.resize(maximum, ShrinkPolicy::Reject).unwrap();
    assert_eq!(writer.len(), maximum);
    writer.write_all_at(maximum - 512, &[61; 512]).unwrap();
    let mut actual = [0; 512];
    writer.read_exact_at(maximum - 512, &mut actual).unwrap();
    assert_eq!(actual, [61; 512]);
    writer.resize(512, ShrinkPolicy::AllowDataLoss).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(VmdkWriter::open(&path).unwrap().len(), 512);
    assert!(fs::metadata(path).unwrap().len() < 4 * 1024 * 1024);
}

#[test]
fn resize_clears_existing_padding_and_retains_cid_after_shifted_extent_line() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    drop(VmdkWriter::create(&path, 512).unwrap());
    let mut bytes = fs::read(&path).unwrap();
    let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
    let table = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
    let payload = u32::from_le_bytes(bytes[table..table + 4].try_into().unwrap()) as usize * 512;
    bytes[payload + 512..payload + 65536].fill(99);
    let descriptor = "# Disk DescriptorFile\nversion=1\nparentCID=ffffffff\ncreateType=\"monolithicSparse\"\nRW 1 SPARSE \"disk.vmdk\"\nCID=ABC\n";
    bytes[512..21 * 512].fill(0);
    bytes[512..512 + descriptor.len()].copy_from_slice(descriptor.as_bytes());
    fs::write(&path, bytes).unwrap();
    let mut writer = VmdkWriter::open(&path).unwrap();
    writer.resize(131072, ShrinkPolicy::Reject).unwrap();
    writer.flush().unwrap();
    writer.write_all_at(1000, &[47; 16]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let reader = Vmdk::open(Arc::new(virtdisk::RawDisk::open(&path).unwrap())).unwrap();
    let mut actual = vec![0; 131072];
    reader.read_exact_at(0, &mut actual).unwrap();
    let mut expected = vec![0; 131072];
    expected[1000..1016].fill(47);
    assert_eq!(actual, expected);
    let bytes = fs::read(path).unwrap();
    let text = std::str::from_utf8(&bytes[512..21 * 512]).unwrap();
    let cid = text
        .lines()
        .find_map(|line| line.strip_prefix("CID="))
        .unwrap();
    assert_eq!(cid.len(), 3);
    assert_ne!(cid, "ABC");
    assert!(text.contains("RW 256 SPARSE"));
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn native_qemu_redundant_tables_grow_and_shrink_match_logical_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    assert!(
        std::process::Command::new("qemu-img")
            .args(["create", "-f", "vmdk"])
            .arg(&path)
            .arg("131072")
            .status()
            .unwrap()
            .success()
    );
    let mut writer = VmdkWriter::open(&path).unwrap();
    writer.write_all_at(0, &[53; 512]).unwrap();
    writer.write_all_at(65530, &[71; 32]).unwrap();
    writer.resize(33554432 + 512, ShrinkPolicy::Reject).unwrap();
    writer.write_all_at(33554432, &[83; 512]).unwrap();
    writer.resize(66048, ShrinkPolicy::AllowDataLoss).unwrap();
    writer.resize(33554432 + 512, ShrinkPolicy::Reject).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let output = directory.path().join("converted.raw");
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vmdk", "-O", "raw"])
            .arg(&path)
            .arg(&output)
            .status()
            .unwrap()
            .success()
    );
    let mut expected = vec![0; 33554432 + 512];
    expected[..512].fill(53);
    expected[65530..65562].fill(71);
    assert_eq!(fs::read(output).unwrap(), expected);
}
