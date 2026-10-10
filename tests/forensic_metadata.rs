#![cfg(feature = "std")]
use virtdisk::{ContainerMetadata, Image, ImageFormat, ImageWriter, InspectMetadata};

#[cfg(target_os = "linux")]
#[test]
fn qcow2_snapshot_timestamps_are_preserved_without_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let mut writer = virtdisk::Qcow2Writer::create(&path, 65536).unwrap();
    writer.create_snapshot(b"id", b"recorded").unwrap();
    drop(writer);
    let mut bytes = std::fs::read(&path).unwrap();
    let at = u64::from_be_bytes(bytes[64..72].try_into().unwrap()) as usize;
    bytes[at + 16..at + 20].copy_from_slice(&42u32.to_be_bytes());
    bytes[at + 20..at + 24].copy_from_slice(&123456789u32.to_be_bytes());
    bytes[at + 24..at + 32].copy_from_slice(&u64::MAX.to_be_bytes());
    std::fs::write(&path, bytes).unwrap();
    let image = Image::open(&path, Some(ImageFormat::Qcow2)).unwrap();
    let ContainerMetadata::Qcow2 { snapshots } = image.metadata().unwrap() else {
        panic!("expected QCOW2")
    };
    assert_eq!(snapshots[0].date_seconds, 42);
    assert_eq!(snapshots[0].date_nanoseconds, 123456789);
    assert_eq!(snapshots[0].vm_clock_nanoseconds, u64::MAX);
}

#[test]
fn vdi_parent_uuid_fields_match_the_authorized_parent_header() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent.vdi");
    let child = directory.path().join("child.vdi");
    drop(ImageWriter::create(&parent, ImageFormat::Vdi, 65536).unwrap());
    virtdisk::create_vdi_overlay(&child, &parent, &[]).unwrap();
    let image = Image::open_chain(
        &child,
        Some(ImageFormat::Vdi),
        std::slice::from_ref(&parent),
    )
    .unwrap();
    let ContainerMetadata::Vdi(value) = image.metadata().unwrap() else {
        panic!("expected VDI")
    };
    let bytes = std::fs::read(parent).unwrap();
    assert_eq!(value.parent_uuid, bytes[392..408]);
    assert_eq!(value.parent_modification_uuid, bytes[408..424]);
}

#[test]
fn dispatch_preserves_vdi_uuid_bytes_and_vhdx_header_fields() {
    let directory = tempfile::tempdir().unwrap();
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vhdx,
    ] {
        let path = directory.path().join(format!("{format:?}"));
        drop(ImageWriter::create(&path, format, 65536).unwrap());
        let bytes = std::fs::read(&path).unwrap();
        let image = Image::open(&path, Some(format)).unwrap();
        let metadata = image.metadata().unwrap();
        match metadata {
            ContainerMetadata::Raw => assert_eq!(format, ImageFormat::Raw),
            ContainerMetadata::Qcow2 { snapshots } => assert!(snapshots.is_empty()),
            ContainerMetadata::Vdi(value) => {
                assert_eq!(value.creation_uuid, bytes[392..408]);
                assert_eq!(value.modification_uuid, bytes[408..424]);
                assert_eq!(value.parent_uuid, [0; 16]);
                assert_eq!(value.parent_modification_uuid, [0; 16]);
            }
            ContainerMetadata::Vhdx(value) => {
                let active = value
                    .headers
                    .iter()
                    .flatten()
                    .max_by_key(|h| h.sequence_number)
                    .unwrap();
                assert_eq!(value.active_header_offset, active.offset);
                for header in value.headers.iter().flatten() {
                    let at = header.offset as usize;
                    assert_eq!(
                        header.sequence_number,
                        u64::from_le_bytes(bytes[at + 8..at + 16].try_into().unwrap())
                    );
                    assert_eq!(header.file_write_guid, bytes[at + 16..at + 32]);
                    assert_eq!(header.data_write_guid, bytes[at + 32..at + 48]);
                    assert_eq!(header.log_guid, bytes[at + 48..at + 64]);
                }
                assert!(value.parent_locator.is_empty());
                assert_ne!(value.virtual_disk_id, [0; 16]);
            }
            _ => panic!("unexpected metadata"),
        }
        // Every public image-opening path retains the metadata provider.
        assert_eq!(
            image.metadata().unwrap(),
            Image::open_chain(&path, Some(format), &[])
                .unwrap()
                .metadata()
                .unwrap()
        );
        let options = virtdisk::ReaderOpenOptions::default().format(format);
        assert_eq!(
            image.metadata().unwrap(),
            Image::open_with_options(&path, &options)
                .unwrap()
                .metadata()
                .unwrap()
        );
    }
}

#[test]
fn vmdk_preserves_vendor_properties_extent_order_and_short_descriptors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let extent = directory.path().join("part=1.raw");
    std::fs::write(&extent, [7; 1024]).unwrap();
    std::fs::write(&path, "version=1\nCID=012a\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\nddb.comment=\"quoted \\\" text\"\nddb.comment=second\nRW 1 FLAT \"part=1.raw\" 1\n").unwrap();
    let image = Image::open_chain(&path, Some(ImageFormat::Vmdk), &[extent]).unwrap();
    let ContainerMetadata::Vmdk(Some(value)) = image.metadata().unwrap() else {
        panic!("expected descriptor")
    };
    assert_eq!(value.cid, 0x12a);
    assert_eq!(value.parent_cid, u32::MAX);
    let comments: Vec<_> = value
        .properties
        .iter()
        .filter(|(key, _)| key == "ddb.comment")
        .collect();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[1].1, "second");
    assert_eq!(value.extents[0].name, "part=1.raw");
    assert_eq!(value.extents[0].sectors, 1);
    assert_eq!(value.extents[0].offset_sectors, Some(1));
}

#[test]
fn vhdx_reports_invalid_inactive_slot_and_preserves_native_parent_locator() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent.vhdx");
    let child = directory.path().join("child.vhdx");
    drop(ImageWriter::create(&parent, ImageFormat::Vhdx, 65536).unwrap());
    virtdisk::create_vhdx_overlay(&child, &parent, &[]).unwrap();
    let image = Image::open_chain(
        &child,
        Some(ImageFormat::Vhdx),
        std::slice::from_ref(&parent),
    )
    .unwrap();
    let ContainerMetadata::Vhdx(value) = image.metadata().unwrap() else {
        panic!("expected VHDX")
    };
    assert!(value.parent_locator.contains_key("parent_linkage"));
    assert!(
        value
            .parent_locator
            .values()
            .any(|v| v.contains("parent.vhdx"))
    );
    drop(image);
    let mut bytes = std::fs::read(&child).unwrap();
    let inactive = if value.active_header_offset == 65536 {
        131072
    } else {
        65536
    };
    bytes[inactive] = 0;
    std::fs::write(&child, bytes).unwrap();
    let image = Image::open_chain(&child, Some(ImageFormat::Vhdx), &[parent]).unwrap();
    let ContainerMetadata::Vhdx(value) = image.metadata().unwrap() else {
        panic!("expected VHDX")
    };
    assert_eq!(value.headers.iter().flatten().count(), 1);
}

#[test]
fn metadata_extraction_respects_retained_budget() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vdi");
    drop(ImageWriter::create(&path, ImageFormat::Vdi, 65536).unwrap());
    let image = Image::open(&path, Some(ImageFormat::Vdi)).unwrap();
    use virtdisk::ReadAt;
    let budget = image.budget().unwrap();
    budget
        .metadata(budget.limits().metadata_bytes - budget.usage().metadata_bytes)
        .unwrap();
    assert_eq!(
        image.metadata().unwrap_err().kind(),
        virtdisk::io::ErrorKind::ResourceLimit
    );
}
