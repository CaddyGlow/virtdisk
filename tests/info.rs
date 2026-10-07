use virtdisk::{
    Capability, ImageOperation, InspectImage, RawWriter, UnsupportedReason, VdiWriter, VmdkWriter,
};
#[test]
fn opened_writer_reports_actual_operations_not_family_promises() {
    let dir = tempfile::tempdir().unwrap();
    let raw = RawWriter::create(dir.path().join("raw"), 512).unwrap();
    let report = raw.inspection();
    assert_eq!(report.geometry.virtual_size, 512);
    assert_eq!(report.geometry.logical_sector_size, None);
    assert_eq!(
        report.capabilities.get(ImageOperation::Resize),
        Capability::Supported
    );
    assert_eq!(
        report.capabilities.get(ImageOperation::Discard),
        if cfg!(target_os = "linux") {
            Capability::Supported
        } else {
            Capability::Unsupported(UnsupportedReason::NotImplemented)
        }
    );
    let vmdk = VmdkWriter::create(dir.path().join("disk.vmdk"), 65536).unwrap();
    assert_eq!(
        vmdk.inspection().geometry.allocation_block_size,
        Some(65536)
    );
    assert_eq!(
        vmdk.inspection().capabilities.get(ImageOperation::Resize),
        if cfg!(target_os = "linux") {
            Capability::Supported
        } else {
            Capability::Unsupported(UnsupportedReason::NotImplemented)
        }
    );
    let vdi = VdiWriter::create(dir.path().join("disk.vdi"), 1048576).unwrap();
    assert_eq!(vdi.inspection().geometry.logical_sector_size, Some(512));
    assert_eq!(
        vdi.inspection()
            .capabilities
            .get(ImageOperation::NativeSnapshot),
        Capability::Unsupported(UnsupportedReason::NotImplemented)
    );
}

#[test]
fn readers_report_readonly_profiles_and_real_vhdx_sector_sizes() {
    use std::{fs, sync::Arc};
    use virtdisk::{
        Qcow2, Qcow2Writer, RawDisk, ReadAt, ValidationLevel, Vhdx, VhdxWriter, Vmdk, create_vhdx,
    };
    let dir = tempfile::tempdir().unwrap();
    let qcow = dir.path().join("disk.qcow2");
    drop(Qcow2Writer::create(&qcow, 65536).unwrap());
    let d = Qcow2::open(Arc::new(RawDisk::open(&qcow).unwrap())).unwrap();
    let r = d.inspection();
    assert_eq!(r.validation, ValidationLevel::MappingBounds);
    assert_eq!(r.native_snapshots, Some(0));
    assert!(!r.has_parent);
    assert_eq!(
        r.capabilities.get(ImageOperation::Write),
        Capability::Unsupported(UnsupportedReason::ReadOnlyHandle)
    );
    assert_eq!(
        r.capabilities.get(ImageOperation::ExtentMap),
        Capability::Supported
    );
    let vmdk = dir.path().join("disk.vmdk");
    drop(VmdkWriter::create(&vmdk, 65536).unwrap());
    let d = Vmdk::open(Arc::new(RawDisk::open(&vmdk).unwrap())).unwrap();
    assert_eq!(
        d.inspection().capabilities.get(ImageOperation::Write),
        Capability::Unsupported(UnsupportedReason::ReadOnlyHandle)
    );
    let raw = dir.path().join("source");
    fs::write(&raw, vec![3; 65536]).unwrap();
    let vhdx = dir.path().join("disk.vhdx");
    create_vhdx(&vhdx, &RawDisk::open(raw).unwrap()).unwrap();
    let mut bytes = fs::read(&vhdx).unwrap();
    bytes[3 * 1048576 + 65584..3 * 1048576 + 65588].copy_from_slice(&4096u32.to_le_bytes());
    fs::write(&vhdx, bytes).unwrap();
    let d = Vhdx::open(Arc::new(RawDisk::open(&vhdx).unwrap())).unwrap();
    assert_eq!(d.len(), 65536);
    assert_eq!(d.inspection().geometry.logical_sector_size, Some(4096));
    assert_eq!(d.inspection().geometry.physical_sector_size, Some(4096));
    assert_eq!(d.inspection().geometry.allocation_block_size, Some(1048576));
    let w = VhdxWriter::open(&vhdx).unwrap();
    assert_eq!(w.inspection().geometry, d.inspection().geometry);
}

#[test]
fn qcow_reader_reports_declared_snapshots_and_authorized_parent_without_mutation_promises() {
    use std::{fs, sync::Arc};
    use virtdisk::{Qcow2, Qcow2Writer, RawDisk};
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.qcow2");
    drop(Qcow2Writer::create(&image, 65536).unwrap());
    let mut bytes = fs::read(&image).unwrap();
    bytes[60..64].copy_from_slice(&1u32.to_be_bytes());
    fs::write(&image, &bytes).unwrap();
    let d = Qcow2::open(Arc::new(RawDisk::open(&image).unwrap())).unwrap();
    assert_eq!(d.inspection().native_snapshots, Some(1));
    assert_eq!(
        d.inspection()
            .capabilities
            .get(ImageOperation::NativeSnapshot),
        Capability::Unsupported(UnsupportedReason::NotImplemented)
    );
    bytes[60..64].fill(0);
    let name = b"parent.raw";
    bytes[8..16].copy_from_slice(&256u64.to_be_bytes());
    bytes[16..20].copy_from_slice(&(name.len() as u32).to_be_bytes());
    bytes[256..256 + name.len()].copy_from_slice(name);
    bytes[104..108].copy_from_slice(&0xe2792acau32.to_be_bytes());
    bytes[108..112].copy_from_slice(&3u32.to_be_bytes());
    bytes[112..115].copy_from_slice(b"raw");
    fs::write(&image, bytes).unwrap();
    let parent = dir.path().join("parent.raw");
    fs::write(&parent, vec![0; 65536]).unwrap();
    let d = Qcow2::open_chain(&image, &[parent]).unwrap();
    assert!(d.inspection().has_parent);
    assert_eq!(
        d.inspection().capabilities.get(ImageOperation::Merge),
        Capability::Unsupported(UnsupportedReason::NotImplemented)
    );
}
#[cfg(target_os = "linux")]
#[test]
fn qcow_snapshot_creation_updates_handle_count_and_resize_capability() {
    use virtdisk::{Capability, ImageOperation, InspectImage, Qcow2Writer};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let mut writer = Qcow2Writer::create(&path, 65536).unwrap();
    assert_eq!(
        writer
            .inspection()
            .capabilities
            .get(ImageOperation::NativeSnapshot),
        Capability::Supported
    );
    writer.create_snapshot(b"1", b"state").unwrap();
    let info = writer.inspection();
    assert_eq!(info.native_snapshots, Some(1));
    assert!(matches!(
        info.capabilities.get(ImageOperation::Resize),
        Capability::Unsupported(_)
    ));
    drop(writer);
    let writer = Qcow2Writer::open(&path).unwrap();
    assert_eq!(writer.inspection().native_snapshots, Some(1));
}
#[cfg(target_os = "linux")]
#[test]
fn snapshot_lifecycle_inspection_reports_specific_operations_and_refreshes_count() {
    use virtdisk::Qcow2Writer;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let mut writer = Qcow2Writer::create_sparse(&path, 65536).unwrap();
    for operation in [
        ImageOperation::NativeSnapshotCreate,
        ImageOperation::NativeSnapshotDelete,
        ImageOperation::NativeSnapshotRevert,
    ] {
        assert_eq!(
            writer.inspection().capabilities.get(operation),
            Capability::Supported
        );
    }
    writer.create_snapshot(b"one", b"state").unwrap();
    writer.revert_snapshot(b"one").unwrap();
    assert_eq!(writer.inspection().native_snapshots, Some(1));
    writer.delete_snapshot(b"one").unwrap();
    let info = writer.inspection();
    assert_eq!(info.native_snapshots, Some(0));
    assert_eq!(
        info.capabilities.get(ImageOperation::Resize),
        Capability::Supported
    );
    assert_eq!(
        info.capabilities.get(ImageOperation::NativeSnapshotDelete),
        Capability::Supported
    );
}
