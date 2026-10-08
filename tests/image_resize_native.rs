#![cfg(feature = "std")]
use virtdisk::{Image, ImageFormat, ImageWriter, ReadAt, ShrinkPolicy, WriteAt};

#[test]
fn common_native_resize_applies_tail_policy_and_zeroes_regrowth() {
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
        if format != ImageFormat::Raw && !cfg!(target_os = "linux") {
            continue;
        }
        let path = directory.path().join(format!("disk{index}"));
        let mut writer = ImageWriter::create_sparse(&path, format, 131072).unwrap();
        writer.write_all_at(0, &[17; 512]).unwrap();
        writer.write_all_at(65536, &[23; 512]).unwrap();
        assert!(writer.resize(65536, ShrinkPolicy::Reject).is_err());
        assert!(writer.resize(65536, ShrinkPolicy::RequireZero).is_err());
        assert_eq!(writer.len(), 131072);
        writer.resize(65536, ShrinkPolicy::AllowDataLoss).unwrap();
        assert_eq!(writer.len(), 65536);
        writer.resize(196608, ShrinkPolicy::Reject).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let image = Image::open(path, Some(format)).unwrap();
        let mut actual = vec![0; 196608];
        image.read_exact_at(0, &mut actual).unwrap();
        let mut expected = vec![0; 196608];
        expected[..512].fill(17);
        assert_eq!(actual, expected);
    }
}

#[test]
fn unsupported_backed_native_resize_rejects_before_container_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vhdx");
    let parent = directory.path().join("parent.vhdx");
    drop(ImageWriter::create(&parent, ImageFormat::Vhdx, 65536).unwrap());
    virtdisk::create_vhdx_overlay(&path, &parent, &[]).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut writer =
        ImageWriter::open_chain(&path, ImageFormat::Vhdx, std::slice::from_ref(&parent)).unwrap();
    assert_eq!(
        writer
            .resize(131072, ShrinkPolicy::Reject)
            .unwrap_err()
            .kind(),
        virtdisk::io::ErrorKind::Unsupported
    );
    drop(writer);
    assert_eq!(std::fs::read(path).unwrap(), original);
}
