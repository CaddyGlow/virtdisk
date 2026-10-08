use virtdisk::{
    Capability, ImageOperation, InspectImage, RawWriter, UnsupportedReason, VdiWriter, VmdkWriter,
};

#[cfg(target_os = "linux")]
#[test]
fn inspection_reports_primary_and_set_eof_for_every_family() {
    use virtdisk::{Image, ImageFormat, ImageWriter, WriteAt};
    let dir = tempfile::tempdir().unwrap();
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let path = dir.path().join(format!("{format:?}"));
        let writer = ImageWriter::create_sparse(&path, format, 2 * 1048576).unwrap();
        let eof = std::fs::metadata(&path).unwrap().len();
        if format != ImageFormat::Raw {
            assert_ne!(eof, writer.len());
        }
        assert_eq!(writer.inspection().container_size, Some(eof), "{format:?}");
        assert_eq!(
            writer.inspection().container_set_size,
            Some(eof),
            "{format:?}"
        );
        drop(writer);
        let reader = Image::open(&path, Some(format)).unwrap();
        assert_eq!(reader.inspection().container_size, Some(eof), "{format:?}");
        assert_eq!(
            reader.inspection().container_set_size,
            Some(eof),
            "{format:?}"
        );
        assert_eq!(reader.info().container_size, eof);
    }
}
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
    assert_eq!(
        info.capabilities.get(ImageOperation::Resize),
        Capability::Supported
    );
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

#[cfg(target_os = "linux")]
#[test]
fn inspection_tracks_allocation_and_physical_tail_changes_without_reopen() {
    use virtdisk::{DiscardPolicy, ImageFormat, ImageWriter, ShrinkPolicy, WriteAt};
    let dir = tempfile::tempdir().unwrap();
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let path = dir.path().join(format!("{format:?}"));
        let mut writer = ImageWriter::create_sparse(&path, format, 2 * 1048576).unwrap();
        let before = writer.inspection().container_size.unwrap();
        writer.write_all_at(0, &[7; 512]).unwrap();
        let after = std::fs::metadata(&path).unwrap().len();
        assert!(after > before, "{format:?} allocation must grow EOF");
        assert_eq!(writer.inspection().container_size, Some(after));
        assert_eq!(writer.inspection().container_set_size, Some(after));
        if format == ImageFormat::Vdi {
            writer
                .discard(0, 1048576, DiscardPolicy::RequireDeallocation)
                .unwrap();
            let discarded = std::fs::metadata(&path).unwrap().len();
            assert!(discarded < after);
            assert_eq!(writer.inspection().container_size, Some(discarded));
            assert_eq!(writer.inspection().container_set_size, Some(discarded));
        } else if matches!(format, ImageFormat::Qcow2 | ImageFormat::Vmdk) {
            writer.resize(1048576, ShrinkPolicy::AllowDataLoss).unwrap();
            assert_eq!(writer.inspection().geometry.virtual_size, 1048576);
            assert_eq!(writer.inspection().container_size, Some(after));
            assert_eq!(writer.inspection().container_set_size, Some(after));
        }
    }
    let path = dir.path().join("raw");
    let raw = RawWriter::create(&path, 1024).unwrap();
    raw.resize(512).unwrap();
    assert_eq!(raw.inspection().container_size, Some(512));
    assert_eq!(raw.inspection().container_set_size, Some(512));
    assert_eq!(raw.inspection().geometry.virtual_size, 512);
    raw.resize(0).unwrap();
    assert_eq!(raw.inspection().container_size, Some(0));
    assert_eq!(raw.inspection().container_set_size, Some(0));
}

#[test]
fn descriptor_sizes_include_full_flat_files_but_keep_primary_dimension() {
    use virtdisk::{Image, ImageFormat};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vmdk");
    let first = dir.path().join("first-flat.vmdk");
    let second = dir.path().join("second-flat.vmdk");
    std::fs::write(&first, vec![17; 2048]).unwrap();
    std::fs::write(&second, vec![23; 3072]).unwrap();
    std::fs::write(&path, "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\nRW 2 FLAT \"first-flat.vmdk\" 1\nRW 2 FLAT \"second-flat.vmdk\" 2\n").unwrap();
    let primary = std::fs::metadata(&path).unwrap().len();
    let set = primary + 2048 + 3072;
    let authorized = [first, second];
    let reader = Image::open_chain(&path, Some(ImageFormat::Vmdk), &authorized).unwrap();
    assert_eq!(reader.info().container_size, primary);
    assert_eq!(reader.inspection().container_size, Some(primary));
    assert_eq!(reader.inspection().container_set_size, Some(set));
    assert_eq!(reader.inspection().geometry.virtual_size, 2048);
    drop(reader);
    let writer = VmdkWriter::open_descriptor(&path, &authorized).unwrap();
    assert_eq!(writer.inspection().container_size, Some(primary));
    assert_eq!(writer.inspection().container_set_size, Some(set));
    writer.write_all_at(1000, &[9; 48]).unwrap();
    assert_eq!(writer.inspection().container_size, Some(primary));
    assert_eq!(writer.inspection().container_set_size, Some(set));
}

#[cfg(target_os = "linux")]
#[test]
fn split_sparse_sizes_track_both_extent_allocations_without_reopen() {
    use virtdisk::{Image, ImageFormat};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vmdk");
    let mut extents = Vec::new();
    for name in ["disk-s1.vmdk", "disk-s2.vmdk"] {
        let extent = dir.path().join(name);
        drop(VmdkWriter::create_sparse(&extent, 65536).unwrap());
        let mut bytes = std::fs::read(&extent).unwrap();
        let offset = u64::from_le_bytes(bytes[28..36].try_into().unwrap()) as usize * 512;
        let size = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize * 512;
        bytes[offset..offset + size].fill(0);
        std::fs::write(&extent, bytes).unwrap();
        extents.push(extent);
    }
    std::fs::write(&path, "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"disk-s1.vmdk\"\nRW 128 SPARSE \"disk-s2.vmdk\"\n").unwrap();
    let primary = std::fs::metadata(&path).unwrap().len();
    let actual_set = || {
        primary
            + extents
                .iter()
                .map(|p| std::fs::metadata(p).unwrap().len())
                .sum::<u64>()
    };
    let before = actual_set();
    let writer = VmdkWriter::open_descriptor(&path, &extents).unwrap();
    assert_eq!(writer.inspection().container_size, Some(primary));
    assert_eq!(writer.inspection().container_set_size, Some(before));
    writer.write_all_at(65535, &[7; 2]).unwrap();
    assert!(actual_set() > before);
    assert_eq!(writer.inspection().container_size, Some(primary));
    assert_eq!(writer.inspection().container_set_size, Some(actual_set()));
    writer.flush().unwrap();
    drop(writer);
    let reader = Image::open_chain(&path, Some(ImageFormat::Vmdk), &extents).unwrap();
    assert_eq!(reader.info().container_size, primary);
    assert_eq!(reader.inspection().container_set_size, Some(actual_set()));
}

#[test]
fn bounded_non_file_container_source_reports_its_own_length() {
    use std::{io, sync::Arc};
    use virtdisk::{Qcow2, Qcow2Writer, ReadAt};
    struct Bytes(Vec<u8>);
    impl ReadAt for Bytes {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_exact_at(&self, at: u64, out: &mut [u8]) -> io::Result<()> {
            let start = usize::try_from(at).map_err(|_| io::ErrorKind::UnexpectedEof)?;
            let end = start
                .checked_add(out.len())
                .ok_or(io::ErrorKind::UnexpectedEof)?;
            out.copy_from_slice(self.0.get(start..end).ok_or(io::ErrorKind::UnexpectedEof)?);
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    drop(Qcow2Writer::create(&path, 65536).unwrap());
    let bytes = std::fs::read(&path).unwrap();
    let length = bytes.len() as u64;
    std::fs::remove_file(&path).unwrap();
    let reader = Qcow2::open(Arc::new(Bytes(bytes))).unwrap();
    assert_eq!(reader.inspection().container_size, Some(length));
    assert_eq!(reader.inspection().container_set_size, Some(length));
}

#[cfg(target_os = "linux")]
#[test]
fn inspection_excludes_authorized_parent_containers_for_every_backed_family() {
    use virtdisk::{Image, ImageFormat, ImageWriter};
    let dir = tempfile::tempdir().unwrap();
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let base = dir.path().join(format!("base{format:?}"));
        let child = dir.path().join(format!("child{format:?}"));
        drop(ImageWriter::create(&base, format, 2 * 1048576).unwrap());
        match format {
            ImageFormat::Qcow2 => {
                virtdisk::create_qcow2_overlay(&child, &base, "qcow2", 2 * 1048576).unwrap()
            }
            ImageFormat::Vdi => virtdisk::create_vdi_overlay(&child, &base, &[]).unwrap(),
            ImageFormat::Vmdk => {
                drop(
                    VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base)).unwrap(),
                );
            }
            ImageFormat::Vhdx => virtdisk::create_vhdx_overlay(&child, &base, &[]).unwrap(),
            ImageFormat::Raw => unreachable!(),
        }
        let own = std::fs::metadata(&child).unwrap().len();
        let writer = ImageWriter::open_chain(&child, format, std::slice::from_ref(&base)).unwrap();
        assert!(writer.inspection().has_parent);
        assert_eq!(writer.inspection().container_size, Some(own));
        assert_eq!(writer.inspection().container_set_size, Some(own));
        drop(writer);
        let reader = Image::open_chain(&child, Some(format), std::slice::from_ref(&base)).unwrap();
        assert!(reader.inspection().has_parent);
        assert_eq!(reader.inspection().container_size, Some(own));
        assert_eq!(reader.inspection().container_set_size, Some(own));
    }
}
