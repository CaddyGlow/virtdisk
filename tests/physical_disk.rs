#![cfg(all(feature = "std", any(target_os = "linux", windows)))]
use virtdisk::{PhysicalDisk, RawDisk, ReadAt};

// Matches both native fixture generators: mix the full 512-byte sector
// number into every eight-byte group, retaining the within-sector pattern.
fn fixture_byte(offset: u64) -> u8 {
    offset as u8 ^ ((offset >> 9) >> (8 * (offset % 8))) as u8
}

#[test]
fn fixture_distinguishes_every_sector() {
    let signatures: std::collections::HashSet<[u8; 8]> = (0..(8 << 20) / 512)
        .map(|sector| std::array::from_fn(|index| fixture_byte(sector * 512 + index as u64)))
        .collect();
    assert_eq!(signatures.len(), (8 << 20) / 512);
}

#[test]
fn rejects_regular_files_and_directories() {
    let file = tempfile::NamedTempFile::new().unwrap();
    assert!(PhysicalDisk::open(file.path()).is_err());
    assert!(PhysicalDisk::open(file.path().parent().unwrap()).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn rejects_character_devices() {
    assert!(PhysicalDisk::open("/dev/null").is_err());
    assert!(PhysicalDisk::open("/dev/zero").is_err());
}

/// The native runner creates a disposable device with distinct sector contents.
/// Missing provisioning is a failure, never an implicit test pass.
#[test]
#[ignore = "requires disposable native device; use scripts/test-physical-disk.*"]
fn native_device_geometry_byte_reads_bounds_and_concurrency() {
    let path = std::env::var_os("VIRTDISK_TEST_DEVICE").expect("native device path required");
    let disk = PhysicalDisk::open(&path).unwrap();
    assert_eq!(disk.len(), 8 << 20);
    let sector = std::env::var("VIRTDISK_TEST_SECTOR_SIZE")
        .unwrap_or_else(|_| "512".into())
        .parse::<u32>()
        .unwrap();
    assert_eq!(disk.logical_sector_size(), sector);
    assert!(RawDisk::open(&path).is_err());
    for (offset, count) in [
        (0, 512),
        (1, 1027),
        (511, 4099),
        (65535, 131073),
        ((8 << 20) - 513, 513),
    ] {
        let mut bytes = vec![0; count];
        disk.read_exact_at(offset, &mut bytes).unwrap();
        for (index, byte) in bytes.into_iter().enumerate() {
            assert_eq!(byte, fixture_byte(offset + index as u64));
        }
    }
    disk.read_exact_at(disk.len(), &mut []).unwrap();
    let mut untouched = [0xa5; 2];
    assert!(disk.read_exact_at(disk.len() - 1, &mut untouched).is_err());
    assert_eq!(untouched, [0xa5; 2]);
    assert!(disk.read_exact_at(u64::MAX, &mut untouched).is_err());
    std::thread::scope(|scope| {
        for offset in [3, 12345, 65535, 777777] {
            let disk = &disk;
            scope.spawn(move || {
                let mut bytes = [0; 1025];
                disk.read_exact_at(offset, &mut bytes).unwrap();
                for (index, byte) in bytes.into_iter().enumerate() {
                    assert_eq!(byte, fixture_byte(offset + index as u64));
                }
            });
        }
    });
}
