#![cfg(feature = "std")]
use std::{fs, sync::Arc};
use virtdisk::{RawDisk, ReadAt, Vdi, VdiWriter};
#[test]
fn fixed_writer_lock_bounds_zeroes_and_modification_identity() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vdi");
    let d = VdiWriter::create(&image, 2 * 1048576 + 512).unwrap();
    assert!(VdiWriter::open(&image).is_err());
    assert_eq!(d.len(), 2 * 1048576 + 512);
    assert!(!d.is_empty());
    // Windows locks prohibit independent physical reads while the writer lives.
    drop(d);
    let before = fs::read(&image).unwrap()[408..424].to_vec();
    let d = VdiWriter::open(&image).unwrap();
    d.write_all_at(1048570, &[7; 20]).unwrap();
    d.write_zeroes(1048575, 4).unwrap();
    d.flush().unwrap();
    drop(d);
    assert_ne!(&fs::read(&image).unwrap()[408..424], before);
    let d = VdiWriter::open(&image).unwrap();
    assert!(d.write_all_at(d.len(), &[]).is_ok());
    assert!(d.write_all_at(d.len(), &[1]).is_err());
    assert!(d.write_all_at(u64::MAX, &[1]).is_err());
    drop(d);
    let reader = Vdi::open(Arc::new(RawDisk::open(&image).unwrap())).unwrap();
    let mut bytes = [0; 20];
    reader.read_exact_at(1048570, &mut bytes).unwrap();
    assert_eq!(&bytes[5..9], &[0; 4]);
    drop(reader);
    let writer = VdiWriter::open(&image).unwrap();
    writer.read_exact_at(1048570, &mut bytes).unwrap();
    assert_eq!(&bytes[..5], &[7; 5]);
}

#[test]
fn aliases_and_parents_rejected_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vdi");
    drop(VdiWriter::create(&image, 2 * 1048576).unwrap());
    let original = fs::read(&image).unwrap();
    for field in [0, 2] {
        let mut bytes = original.clone();
        match field {
            0 => bytes[516..520].copy_from_slice(&0u32.to_le_bytes()),
            _ => bytes[424] = 1,
        };
        fs::write(&image, &bytes).unwrap();
        assert!(VdiWriter::open(&image).is_err());
        assert_eq!(fs::read(&image).unwrap(), bytes);
    }
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_post_write_matches_payload() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vdi");
    let raw = dir.path().join("raw");
    let d = VdiWriter::create(&image, 2 * 1048576 + 512).unwrap();
    let mut expected = vec![0; 2 * 1048576 + 512];
    expected[1048500..1048700].fill(55);
    d.write_all_at(1048500, &expected[1048500..1048700])
        .unwrap();
    d.flush().unwrap();
    drop(d);
    let first = fs::read(&image).unwrap()[408..424].to_vec();
    let d = VdiWriter::open(&image).unwrap();
    d.write_zeroes(1048600, 50).unwrap();
    expected[1048600..1048650].fill(0);
    d.flush().unwrap();
    drop(d);
    assert_ne!(fs::read(&image).unwrap()[408..424], first);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vdi", "-O", "raw"])
            .arg(&image)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(raw).unwrap(), expected);
}

#[test]
fn read_only_operations_and_empty_writes_preserve_uuid_and_threads_serialize() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vdi");
    let d = Arc::new(VdiWriter::create(&image, 1048576).unwrap());
    drop(d);
    let identity = fs::read(&image).unwrap()[408..424].to_vec();
    let d = Arc::new(VdiWriter::open(&image).unwrap());
    d.write_all_at(0, &[]).unwrap();
    d.write_zeroes(0, 0).unwrap();
    let mut b = [0; 4];
    d.read_exact_at(0, &mut b).unwrap();
    d.flush().unwrap();
    drop(d);
    assert_eq!(fs::read(&image).unwrap()[408..424], identity);
    let d = Arc::new(VdiWriter::open(&image).unwrap());
    std::thread::scope(|scope| {
        for i in 0..8u64 {
            let d = d.clone();
            scope.spawn(move || {
                d.write_all_at(i * 512, &[i as u8; 512]).unwrap();
            });
        }
    });
    for i in 0..8u64 {
        let mut data = [0; 512];
        d.read_exact_at(i * 512, &mut data).unwrap();
        assert_eq!(data, [i as u8; 512]);
    }
}

#[test]
fn fully_allocated_dynamic_profile_is_writable() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.vdi");
    drop(VdiWriter::create(&image, 1048576).unwrap());
    let mut bytes = fs::read(&image).unwrap();
    bytes[76..80].copy_from_slice(&1u32.to_le_bytes());
    fs::write(&image, bytes).unwrap();
    let d = VdiWriter::open(&image).unwrap();
    d.write_all_at(0, &[3; 4]).unwrap();
    d.flush().unwrap();
    let mut out = [0; 4];
    d.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [3; 4]);
}

#[test]
#[cfg(target_os = "linux")]
fn sparse_dynamic_writes_allocate_zero_padded_private_blocks_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("sparse.vdi");
    let d = VdiWriter::create_sparse(&image, 3 * 1048576 + 512).unwrap();
    let initial = fs::metadata(&image).unwrap().len();
    let mut data = [1; 20];
    d.read_exact_at(1048570, &mut data).unwrap();
    assert_eq!(data, [0; 20]);
    d.write_all_at(1048570, &[9; 20]).unwrap();
    d.write_all_at(3 * 1048576, &[6; 512]).unwrap();
    d.write_zeroes(2 * 1048576, 512).unwrap();
    d.flush().unwrap();
    drop(d);
    assert_eq!(fs::metadata(&image).unwrap().len(), initial + 3 * 1048576);
    let d = VdiWriter::open(&image).unwrap();
    d.read_exact_at(1048570, &mut data).unwrap();
    assert_eq!(data, [9; 20]);
    let mut zero = [1; 512];
    d.read_exact_at(2 * 1048576, &mut zero).unwrap();
    assert_eq!(zero, [0; 512]);
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_reads_sparse_dynamic_allocations_and_preserved_holes() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("sparse.vdi");
    let raw = dir.path().join("raw");
    let d = VdiWriter::create_sparse(&image, 3 * 1048576 + 512).unwrap();
    let mut expected = vec![0; 3 * 1048576 + 512];
    expected[1048570..1048590].fill(12);
    expected[3 * 1048576..].fill(23);
    d.write_all_at(1048570, &[12; 20]).unwrap();
    d.write_all_at(3 * 1048576, &[23; 512]).unwrap();
    d.flush().unwrap();
    drop(d);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vdi", "-O", "raw"])
            .arg(image)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(raw).unwrap(), expected);
}

#[test]
#[cfg(target_os = "linux")]
fn sparse_allocation_rejects_hardlinks_foreign_tail_and_corrupt_journal_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("sparse.vdi");
    let alias = dir.path().join("alias.vdi");
    let d = VdiWriter::create_sparse(&image, 1048576).unwrap();
    fs::hard_link(&image, &alias).unwrap();
    let before = fs::read(&image).unwrap();
    assert!(d.write_all_at(0, &[1]).is_err());
    assert_eq!(fs::read(&image).unwrap(), before);
    drop(d);
    let second = dir.path().join("tail.vdi");
    drop(VdiWriter::create_sparse(&second, 1048576).unwrap());
    let mut before = fs::read(&second).unwrap();
    before.push(1);
    fs::write(&second, &before).unwrap();
    let d = VdiWriter::open(&second).unwrap();
    assert!(d.write_all_at(0, &[1]).is_err());
    assert_eq!(fs::read(&second).unwrap(), before);
    drop(d);
    let third = dir.path().join("journal.vdi");
    drop(VdiWriter::create_sparse(&third, 1048576).unwrap());
    let before = fs::read(&third).unwrap();
    fs::write(
        dir.path().join("journal.vdi.virtdisk-transaction"),
        b"invalid",
    )
    .unwrap();
    assert!(VdiWriter::open(&third).is_err());
    assert_eq!(fs::read(&third).unwrap(), before);
    assert!(Vdi::open(Arc::new(RawDisk::open(&third).unwrap())).is_err());
}

#[test]
#[cfg(target_os = "linux")]
fn explicit_zero_blocks_and_concurrent_sparse_writes_allocate_unique_owners() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("sparse.vdi");
    drop(VdiWriter::create_sparse(&image, 2 * 1048576).unwrap());
    let mut bytes = fs::read(&image).unwrap();
    bytes[512..516].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
    fs::write(&image, bytes).unwrap();
    let d = Arc::new(VdiWriter::open(&image).unwrap());
    std::thread::scope(|scope| {
        for index in 0..2u64 {
            let d = d.clone();
            scope.spawn(move || {
                d.write_all_at(index * 1048576 + 17, &[index as u8 + 3; 19])
                    .unwrap();
            });
        }
    });
    d.flush().unwrap();
    drop(d);
    let d = Vdi::open(Arc::new(RawDisk::open(&image).unwrap())).unwrap();
    for index in 0..2u64 {
        let mut data = [1; 64];
        d.read_exact_at(index * 1048576, &mut data).unwrap();
        assert_eq!(&data[..17], &[0; 17]);
        assert_eq!(&data[17..36], &[index as u8 + 3; 19]);
        assert_eq!(&data[36..], &[0; 28]);
    }
}

#[test]
#[cfg(not(target_os = "linux"))]
fn sparse_creation_refuses_unsupported_host_before_creating_output() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("unsupported.vdi");
    let error = VdiWriter::create_sparse(&image, 1048576).err().unwrap();
    assert_eq!(error.kind(), virtdisk::io::ErrorKind::Unsupported);
    assert!(!image.exists());
}
