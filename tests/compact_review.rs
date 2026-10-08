#![cfg(feature = "std")]
#[test]
#[ignore = "requires independent qemu-img oracle"]
fn published_vmdk_compaction_is_readable_after_staging_filename_changes() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.raw");
    let output = dir.path().join("published.vmdk");
    let flat = dir.path().join("flat.raw");
    let expected = vec![7; 65536];
    std::fs::write(&source, &expected).unwrap();
    virtdisk::compact_image(
        &virtdisk::RawDisk::open(&source).unwrap(),
        &output,
        virtdisk::ImageFormat::Vmdk,
    )
    .unwrap();
    let result = std::process::Command::new("qemu-img")
        .args(["convert", "-f", "vmdk", "-O", "raw"])
        .arg(&output)
        .arg(&flat)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(flat).unwrap(), expected);
}
