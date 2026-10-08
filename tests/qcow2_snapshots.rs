#![cfg(feature = "std")]
use std::{fs, sync::Arc};
use virtdisk::{Qcow2, Qcow2Writer, RawDisk};

fn fixture(path: &std::path::Path) -> Vec<u8> {
    drop(Qcow2Writer::create(path, 131072).unwrap());
    let mut bytes = fs::read(path).unwrap();
    let offset = bytes.len();
    let l1 = bytes[40..48].to_vec();
    bytes[60..64].copy_from_slice(&1u32.to_be_bytes());
    bytes[64..72].copy_from_slice(&(offset as u64).to_be_bytes());
    let mut entry = [0; 64];
    entry[..8].copy_from_slice(&l1);
    entry[8..12].copy_from_slice(&1u32.to_be_bytes());
    entry[12..14].copy_from_slice(&1u16.to_be_bytes());
    entry[14..16].copy_from_slice(&5u16.to_be_bytes());
    entry[16..20].copy_from_slice(&42u32.to_be_bytes());
    entry[20..24].copy_from_slice(&17u32.to_be_bytes());
    entry[36..40].copy_from_slice(&16u32.to_be_bytes());
    entry[48..56].copy_from_slice(&131072u64.to_be_bytes());
    entry[56..62].copy_from_slice(b"1state");
    bytes.extend_from_slice(&entry);
    bytes
}

#[test]
fn bounded_listing_reports_disk_and_vm_metadata_without_enabling_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk");
    fs::write(&path, fixture(&path)).unwrap();
    let reader = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let snapshots = reader.list_snapshots().unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].id, b"1");
    assert_eq!(snapshots[0].name, b"state");
    assert_eq!(snapshots[0].virtual_size, 131072);
    assert_eq!(snapshots[0].vm_state_size, 0);
    assert_eq!(snapshots[0].date_seconds, 42);
    assert_eq!(snapshots[0].date_nanoseconds, 17);
    assert!(reader.validate_active_mapping().is_err());
}

#[test]
#[ignore = "requires independent qemu-img/qemu-io snapshot content oracle"]
fn native_snapshot_views_preserve_saved_bytes_and_current_state() {
    use std::process::Command;
    use virtdisk::ReadAt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk");
    assert!(
        Command::new("qemu-img")
            .args(["create", "-f", "qcow2"])
            .arg(&path)
            .arg("128K")
            .status()
            .unwrap()
            .success()
    );
    let write = |pattern: u8, offset: u64| {
        assert!(
            Command::new("qemu-io")
                .args([
                    "-f",
                    "qcow2",
                    "-c",
                    &format!("write -P {pattern} {offset} 65536")
                ])
                .arg(&path)
                .output()
                .unwrap()
                .status
                .success()
        )
    };
    write(17, 0);
    assert!(
        Command::new("qemu-img")
            .args(["snapshot", "-c", "before"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    write(23, 0);
    assert!(
        Command::new("qemu-img")
            .args(["snapshot", "-c", "after"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    write(31, 65536);
    let preliminary = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    let metadata = preliminary.list_snapshots().unwrap();
    drop(preliminary);
    let mut original = fs::read(&path).unwrap();
    let word = |bytes: &[u8], offset: usize| {
        u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
    };
    let l2 =
        (word(&original, metadata[0].l1_table_offset as usize) & 0x00ff_ffff_ffff_fe00) as usize;
    assert_eq!(word(&original, l2 + 8), 0);
    original[l2 + 8..l2 + 16].copy_from_slice(&(1u64 << 63).to_be_bytes());
    fs::write(&path, &original).unwrap();
    let reader = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
    let snapshots = reader.list_snapshots().unwrap();
    for (index, pattern) in [17, 23].into_iter().enumerate() {
        let view = reader.open_snapshot(&snapshots[index].id).unwrap();
        assert_eq!(view.len(), 131072);
        let mut actual = vec![0; 131072];
        view.read_exact_at(0, &mut actual).unwrap();
        let mut expected = vec![0; 131072];
        expected[..65536].fill(pattern);
        assert_eq!(actual, expected);
        let output = directory.path().join(format!("snapshot{index}.raw"));
        virtdisk::convert_image(&view, &output, virtdisk::ImageFormat::Raw).unwrap();
        assert_eq!(fs::read(output).unwrap(), expected);
    }
    assert!(reader.open_snapshot(b"unknown").is_err());
    let mut active = [0; 1];
    reader.read_exact_at(65536, &mut active).unwrap();
    assert_eq!(active, [31]);
    assert_eq!(fs::read(&path).unwrap(), original);
    drop(reader);
    let data = word(&original, l2) & 0x00ff_ffff_ffff_fe00;
    let ref_table = word(&original, 48) as usize;
    let ref_block = word(&original, ref_table) as usize;
    let counter = ref_block + (data as usize / 65536) * 2;
    original[counter..counter + 2].fill(0);
    fs::write(&path, original).unwrap();
    let corrupt = Arc::new(Qcow2::open(Arc::new(RawDisk::open(path).unwrap())).unwrap());
    assert!(corrupt.open_snapshot(&metadata[0].id).is_err());
}

#[test]
fn malformed_snapshot_tables_reject_bounds_counts_and_partial_v3_extra_fields() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk");
    let original = fixture(&path);
    let table = original.len() - 64;
    for variant in 0..5 {
        let mut bytes = original.clone();
        match variant {
            0 => bytes[60..64].copy_from_slice(&1025u32.to_be_bytes()),
            1 => bytes[64..72].copy_from_slice(&1u64.to_be_bytes()),
            2 => bytes[table + 36..table + 40].copy_from_slice(&8u32.to_be_bytes()),
            3 => bytes[table + 20..table + 24].copy_from_slice(&1000000000u32.to_be_bytes()),
            _ => {
                bytes.truncate(table + 61);
            }
        }
        fs::write(&path, bytes).unwrap();
        let reader = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
        assert!(reader.list_snapshots().is_err(), "variant {variant}");
    }
}

#[test]
#[ignore = "requires independent qemu-img snapshot metadata oracle"]
fn lists_native_qemu_v2_and_v3_snapshots_with_distinct_saved_capacities() {
    use std::process::Command;
    for compatibility in ["0.10", "1.1"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("disk.qcow2");
        assert!(
            Command::new("qemu-img")
                .args([
                    "create",
                    "-f",
                    "qcow2",
                    "-o",
                    &format!("compat={compatibility}")
                ])
                .arg(&path)
                .arg("128K")
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("qemu-img")
                .args(["snapshot", "-c", "before"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        if compatibility == "1.1" {
            assert!(
                Command::new("qemu-img")
                    .args(["resize", "--shrink"])
                    .arg(&path)
                    .arg("64K")
                    .status()
                    .unwrap()
                    .success()
            );
        }
        assert!(
            Command::new("qemu-img")
                .args(["snapshot", "-c", "after"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let reader = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
        let snapshots = reader.list_snapshots().unwrap();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].name, b"before");
        assert_eq!(snapshots[1].name, b"after");
        assert_ne!(snapshots[0].id, snapshots[1].id);
        assert_eq!(snapshots[0].virtual_size, 131072);
        assert_eq!(
            snapshots[1].virtual_size,
            if compatibility == "1.1" {
                65536
            } else {
                131072
            }
        );
        assert!(snapshots.iter().all(|s| s.vm_state_size == 0));
    }
}
