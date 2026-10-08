#![cfg(feature = "std")]
use std::{fs, sync::Arc};
use virtdisk::{RawDisk, ReadAt, Vmdk, create_vmdk};
#[test]
fn creates_sparse_image_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("source.raw");
    let mut bytes = vec![0; 196608];
    bytes[70000..80000].fill(7);
    fs::write(&raw, &bytes).unwrap();
    let source = RawDisk::open(&raw).unwrap();
    let image = dir.path().join("disk.vmdk");
    create_vmdk(&image, &source).unwrap();
    let reader = Vmdk::open(Arc::new(RawDisk::open(&image).unwrap())).unwrap();
    let mut actual = vec![0; bytes.len()];
    reader.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, bytes);
    assert!(fs::metadata(&image).unwrap().len() < bytes.len() as u64);
    assert!(create_vmdk(&image, &source).is_err());
}
#[test]
#[ignore = "requires independent qemu-img oracle"]
fn exported_image_matches_qemu_raw_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw");
    let image = dir.path().join("disk.vmdk");
    let output = dir.path().join("converted.raw");
    let mut bytes = vec![0; 196608 + 512];
    bytes[17..90000].fill(23);
    fs::write(&raw, &bytes).unwrap();
    create_vmdk(&image, &RawDisk::open(&raw).unwrap()).unwrap();
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vmdk", "-O", "raw"])
            .arg(&image)
            .arg(&output)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(output).unwrap(), bytes);
}

#[test]
fn invalid_geometry_never_creates_output_and_failed_export_stays_dirty() {
    use virtdisk::io;
    struct Broken;
    impl ReadAt for Broken {
        fn len(&self) -> u64 {
            512
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::other("injected read failure"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("unaligned");
    fs::write(&raw, [1]).unwrap();
    let image = dir.path().join("disk.vmdk");
    assert!(create_vmdk(&image, &RawDisk::open(&raw).unwrap()).is_err());
    assert!(!image.exists());
    assert!(create_vmdk(&image, &Broken).is_err());
    assert!(Vmdk::open(Arc::new(RawDisk::open(&image).unwrap())).is_err());
    assert_eq!(fs::read(&image).unwrap()[72], 1);
}
