#![cfg(feature = "std")]
use virtdisk::{Image, ImageFormat, ImageWriter, InspectImage, ReadAt, WriteAt};

#[test]
fn creation_dispatch_retains_lock_and_publishes_zero_readable_profiles() {
    let directory = tempfile::tempdir().unwrap();
    for (index, format) in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("disk{index}"));
        let writer = ImageWriter::create(&path, format, 131072).unwrap();
        assert_eq!(writer.len(), 131072);
        assert_eq!(writer.inspection().geometry.virtual_size, 131072);
        assert!(ImageWriter::open(&path, format).is_err());
        assert!(ImageWriter::create(&path, format, 512).is_err());
        writer.write_all_at(65530, &[19; 20]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let image = Image::open(&path, Some(format)).unwrap();
        let mut expected = vec![0; 131072];
        expected[65530..65550].fill(19);
        let mut actual = vec![0; 131072];
        image.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
fn container_preallocation_is_explicitly_unavailable_without_mutating_payload() {
    let directory = tempfile::tempdir().unwrap();
    let writer =
        ImageWriter::create(directory.path().join("vdi"), ImageFormat::Vdi, 65536).unwrap();
    writer.write_all_at(0, &[17; 512]).unwrap();
    assert_eq!(
        writer.preallocate(0, 65536).unwrap_err().kind(),
        virtdisk::io::ErrorKind::Unsupported
    );
    let mut actual = [0; 512];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, [17; 512]);
}

#[cfg(target_os = "linux")]
#[test]
fn sparse_creation_dispatches_unallocated_profiles_and_allows_native_vdi_resize() {
    use virtdisk::{Capability, ImageOperation, ShrinkPolicy};
    let directory = tempfile::tempdir().unwrap();
    for (index, format) in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("sparse{index}"));
        let mut writer = ImageWriter::create_sparse(&path, format, 2097152).unwrap();
        assert!(
            std::fs::metadata(&path).unwrap().len() < 2097152
                || format == ImageFormat::Raw
                || format == ImageFormat::Vhdx
        );
        assert!(ImageWriter::open(&path, format).is_err());
        if format == ImageFormat::Vdi {
            assert_eq!(
                writer.inspection().capabilities.get(ImageOperation::Resize),
                Capability::Supported
            );
            writer.resize(3145728, ShrinkPolicy::Reject).unwrap();
        }
        writer.write_all_at(1048570, &[37; 16]).unwrap();
        writer.flush().unwrap();
        let size = writer.len();
        drop(writer);
        let image = Image::open(path, Some(format)).unwrap();
        let mut bytes = vec![0; size as usize];
        image.read_exact_at(0, &mut bytes).unwrap();
        let mut expected = vec![0; size as usize];
        expected[1048570..1048586].fill(37);
        assert_eq!(bytes, expected);
    }
}
