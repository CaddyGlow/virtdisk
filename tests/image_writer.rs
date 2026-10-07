use virtdisk::{
    Capability, DiscardPolicy, DiscardResult, ImageFormat, ImageOperation, ImageWriter,
    InspectImage, WriteAt,
};

#[cfg(target_os = "linux")]
#[test]
fn common_writer_reverts_and_deletes_native_snapshot_without_losing_survivors() {
    use std::sync::Arc;
    use virtdisk::{Qcow2, RawDisk, ReadAt};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let mut writer = ImageWriter::create_sparse(&path, ImageFormat::Qcow2, 131072).unwrap();
    writer.write_all_at(10, b"first!").unwrap();
    writer.create_snapshot(b"first", b"first state").unwrap();
    writer.write_all_at(10, b"second").unwrap();
    writer.create_snapshot(b"second", b"second state").unwrap();
    writer.write_all_at(10, b"latest").unwrap();
    assert_eq!(writer.revert_snapshot(b"first").unwrap().id, b"first");
    let mut bytes = [0; 6];
    writer.read_exact_at(10, &mut bytes).unwrap();
    assert_eq!(&bytes, b"first!");
    writer.write_all_at(10, b"edited").unwrap();
    assert_eq!(writer.delete_snapshot(b"first").unwrap().id, b"first");
    writer.flush().unwrap();
    drop(writer);
    let image = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
    image.validate_active_mapping().unwrap();
    assert_eq!(image.list_snapshots().unwrap().len(), 1);
    image
        .open_snapshot(b"second")
        .unwrap()
        .read_exact_at(10, &mut bytes)
        .unwrap();
    assert_eq!(&bytes, b"second");
    image.read_exact_at(10, &mut bytes).unwrap();
    assert_eq!(&bytes, b"edited");
    drop(image);
    let raw = directory.path().join("raw");
    let mut writer = ImageWriter::create(&raw, ImageFormat::Raw, 512).unwrap();
    let original = std::fs::read(&raw).unwrap();
    assert_eq!(
        writer.delete_snapshot(b"x").unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(
        writer.revert_snapshot(b"x").unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&raw).unwrap(), original);
}
#[test]
fn explicit_writer_factory_retains_lock_checks_bounds_and_dispatches_parent_chains() {
    let directory = tempfile::tempdir().unwrap();
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let path = directory.path().join(format!("image-{format:?}"));
        let source_path = directory.path().join(format!("source-{format:?}.raw"));
        std::fs::write(&source_path, vec![7; 1048576]).unwrap();
        virtdisk::convert_image(
            &virtdisk::RawDisk::open(source_path).unwrap(),
            &path,
            format,
        )
        .unwrap();
        let writer = ImageWriter::open(&path, format).unwrap();
        assert!(ImageWriter::open(&path, format).is_err());
        writer.write_all_at(10, &[9; 10]).unwrap();
        assert!(writer.write_all_at(1048575, &[1; 2]).is_err());
        let mut bytes = [0; 30];
        writer.read_exact_at(0, &mut bytes).unwrap();
        let mut expected = [7; 30];
        expected[10..20].fill(9);
        assert_eq!(bytes, expected);
        assert_eq!(
            writer.inspection().capabilities.get(ImageOperation::Write),
            Capability::Supported
        );
        writer.flush().unwrap();
    }
}
#[test]
fn writer_factory_reports_parent_and_applies_discard_policy() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("base.raw");
    let child = directory.path().join("child.qcow2");
    std::fs::write(&raw, vec![7; 65536]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &raw, "raw", 65536).unwrap();
    assert!(ImageWriter::open(&child, ImageFormat::Qcow2).is_err());
    let writer =
        ImageWriter::open_chain(&child, ImageFormat::Qcow2, std::slice::from_ref(&raw)).unwrap();
    assert!(writer.inspection().has_parent);
    assert_eq!(
        writer
            .discard(0, 65536, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    writer.flush().unwrap();
    let mut bytes = [1; 512];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 512]);
    assert_eq!(std::fs::read(raw).unwrap(), vec![7; 65536]);
}

#[test]
#[cfg(target_os = "linux")]
fn all_native_parent_writers_report_parent_retain_lock_and_preserve_cow_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let capacity = 2 * 1048576 + 512;
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let source = directory.path().join(format!("source-{format:?}.raw"));
        let base = directory.path().join(format!("parent-{format:?}"));
        let child = directory.path().join(format!("child-{format:?}"));
        let original = vec![37; capacity];
        std::fs::write(&source, &original).unwrap();
        virtdisk::convert_image(&virtdisk::RawDisk::open(&source).unwrap(), &base, format).unwrap();
        match format {
            ImageFormat::Qcow2 => {
                virtdisk::create_qcow2_overlay(&child, &base, "qcow2", capacity as u64).unwrap()
            }
            ImageFormat::Vdi => virtdisk::create_vdi_overlay(&child, &base, &[]).unwrap(),
            ImageFormat::Vmdk => drop(
                virtdisk::VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base))
                    .unwrap(),
            ),
            ImageFormat::Vhdx => virtdisk::create_vhdx_overlay(&child, &base, &[]).unwrap(),
            ImageFormat::Raw => unreachable!(),
        }
        let parent_bytes = std::fs::read(&base).unwrap();
        let child_bytes = std::fs::read(&child).unwrap();
        assert!(
            ImageWriter::open(&child, format).is_err(),
            "{format:?} unresolved parent"
        );
        assert!(
            ImageWriter::open_chain(&child, format, &[]).is_err(),
            "{format:?} unauthorized parent"
        );
        assert_eq!(
            std::fs::read(&child).unwrap(),
            child_bytes,
            "{format:?} failed open mutated child"
        );
        let writer = ImageWriter::open_chain(&child, format, std::slice::from_ref(&base)).unwrap();
        let inspection = writer.inspection();
        assert!(inspection.has_parent, "{format:?} missing parent report");
        assert_eq!(
            inspection.capabilities.get(ImageOperation::Write),
            Capability::Supported,
            "{format:?}"
        );
        assert_eq!(writer.len(), capacity as u64);
        assert_eq!(
            virtdisk::RawWriter::open(&child).err().unwrap().kind(),
            std::io::ErrorKind::WouldBlock,
            "{format:?} dropped child lock during parent inspection"
        );
        assert!(ImageWriter::open_chain(&child, format, std::slice::from_ref(&base)).is_err());
        let mut observed = vec![0; capacity];
        writer.read_exact_at(0, &mut observed).unwrap();
        assert_eq!(observed, original, "{format:?} initial inheritance");
        let mut expected = original;
        writer.write_zeroes(1048572, 13).unwrap();
        expected[1048572..1048585].fill(0);
        writer.write_all_at(2 * 1048576 - 7, &[91; 23]).unwrap();
        expected[2 * 1048576 - 7..2 * 1048576 + 16].fill(91);
        writer.write_zeroes(capacity as u64 - 29, 13).unwrap();
        expected[capacity - 29..capacity - 16].fill(0);
        assert!(writer.write_all_at(capacity as u64 - 1, &[1; 2]).is_err());
        assert!(writer.write_zeroes(capacity as u64, 1).is_err());
        writer.read_exact_at(0, &mut observed).unwrap();
        assert_eq!(
            observed, expected,
            "{format:?} COW changed bytes outside requested ranges"
        );
        assert!(
            writer.inspection().has_parent,
            "{format:?} lost parent report after allocation"
        );
        writer.flush().unwrap();
        drop(writer);
        assert_eq!(
            std::fs::read(&base).unwrap(),
            parent_bytes,
            "{format:?} modified parent file"
        );
        let writer = ImageWriter::open_chain(&child, format, std::slice::from_ref(&base)).unwrap();
        assert!(
            writer.inspection().has_parent,
            "{format:?} reopened parent report"
        );
        writer.read_exact_at(0, &mut observed).unwrap();
        assert_eq!(observed, expected, "{format:?} reopened COW bytes");
    }
}

#[test]
#[cfg(target_os = "linux")]
fn native_writer_inspection_and_cow_resolve_two_authorized_ancestors() {
    let directory = tempfile::tempdir().unwrap();
    let capacity = 1048576 + 512;
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let source = directory
            .path()
            .join(format!("nested-source-{format:?}.raw"));
        let base = directory.path().join(format!("nested-base-{format:?}"));
        let parent = directory.path().join(format!("nested-parent-{format:?}"));
        let child = directory.path().join(format!("nested-child-{format:?}"));
        let mut expected = vec![18; capacity];
        std::fs::write(&source, &expected).unwrap();
        virtdisk::convert_image(&virtdisk::RawDisk::open(&source).unwrap(), &base, format).unwrap();
        match format {
            ImageFormat::Qcow2 => {
                virtdisk::create_qcow2_overlay(&parent, &base, "qcow2", capacity as u64).unwrap()
            }
            ImageFormat::Vdi => virtdisk::create_vdi_overlay(&parent, &base, &[]).unwrap(),
            ImageFormat::Vmdk => drop(
                virtdisk::VmdkWriter::create_overlay(&parent, &base, std::slice::from_ref(&base))
                    .unwrap(),
            ),
            ImageFormat::Vhdx => virtdisk::create_vhdx_overlay(&parent, &base, &[]).unwrap(),
            ImageFormat::Raw => unreachable!(),
        }
        let writer = ImageWriter::open_chain(&parent, format, std::slice::from_ref(&base)).unwrap();
        writer.write_all_at(777, &[44; 12]).unwrap();
        expected[777..789].fill(44);
        writer.flush().unwrap();
        drop(writer);
        let ancestors = [parent.clone(), base.clone()];
        match format {
            ImageFormat::Qcow2 => virtdisk::create_qcow2_overlay_with_chain(
                &child,
                &parent,
                "qcow2",
                capacity as u64,
                std::slice::from_ref(&base),
            )
            .unwrap(),
            ImageFormat::Vdi => {
                virtdisk::create_vdi_overlay(&child, &parent, std::slice::from_ref(&base)).unwrap()
            }
            ImageFormat::Vmdk => {
                drop(virtdisk::VmdkWriter::create_overlay(&child, &parent, &ancestors).unwrap())
            }
            ImageFormat::Vhdx => {
                virtdisk::create_vhdx_overlay(&child, &parent, std::slice::from_ref(&base)).unwrap()
            }
            ImageFormat::Raw => unreachable!(),
        }
        let originals = [
            std::fs::read(&parent).unwrap(),
            std::fs::read(&base).unwrap(),
        ];
        let child_before = std::fs::read(&child).unwrap();
        assert!(
            ImageWriter::open_chain(&child, format, std::slice::from_ref(&parent)).is_err(),
            "{format:?} missing base authorization"
        );
        assert_eq!(std::fs::read(&child).unwrap(), child_before);
        let writer = ImageWriter::open_chain(&child, format, &ancestors).unwrap();
        assert!(
            writer.inspection().has_parent,
            "{format:?} nested parent report"
        );
        assert_eq!(writer.inspection().geometry.virtual_size, capacity as u64);
        assert_eq!(
            virtdisk::RawWriter::open(&child).err().unwrap().kind(),
            std::io::ErrorKind::WouldBlock,
            "{format:?} nested child lock"
        );
        let mut observed = vec![0; capacity];
        writer.read_exact_at(0, &mut observed).unwrap();
        assert_eq!(
            observed, expected,
            "{format:?} inherited parent modifications"
        );
        writer.write_zeroes(782, 3).unwrap();
        expected[782..785].fill(0);
        writer.write_all_at(1048579, &[62; 8]).unwrap();
        expected[1048579..1048587].fill(62);
        writer.flush().unwrap();
        drop(writer);
        let writer = ImageWriter::open_chain(&child, format, &ancestors).unwrap();
        writer.read_exact_at(0, &mut observed).unwrap();
        assert_eq!(observed, expected, "{format:?} nested COW bytes");
        assert!(writer.inspection().has_parent);
        assert_eq!(std::fs::read(&parent).unwrap(), originals[0]);
        assert_eq!(std::fs::read(&base).unwrap(), originals[1]);
    }
}
#[cfg(target_os = "linux")]
#[test]
fn common_writer_snapshot_creation_preserves_saved_bytes_and_rejects_other_formats() {
    use std::sync::Arc;
    use virtdisk::{ImageFormat, ImageWriter, Qcow2, RawDisk, ReadAt, WriteAt};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let mut writer = ImageWriter::create(&path, ImageFormat::Qcow2, 65536).unwrap();
    writer.write_all_at(0, &[42; 512]).unwrap();
    writer.create_snapshot(b"1", b"saved").unwrap();
    writer.write_all_at(0, &[17; 512]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let reader = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
    let view = reader.open_snapshot(b"1").unwrap();
    let mut bytes = [0; 512];
    view.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [42; 512]);
    reader.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [17; 512]);
    let raw = directory.path().join("raw");
    let mut writer = ImageWriter::create(&raw, ImageFormat::Raw, 512).unwrap();
    let before = std::fs::read(&raw).unwrap();
    assert_eq!(
        writer.create_snapshot(b"1", b"saved").unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&raw).unwrap(), before);
}
