#![cfg(feature = "std")]
use virtdisk::{Image, ImageFormat, InspectImage, ReadAt, VdiWriter};
#[test]
fn dispatch_opens_only_explicit_ordered_native_parents() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vdi");
    let child = dir.path().join("child.vdi");
    let nested = dir.path().join("nested.vdi");
    let writer = VdiWriter::create(&base, 1048576).unwrap();
    writer.write_all_at(5, &[9; 8]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    drop(VdiWriter::create_overlay(&child, &base, &[]).unwrap());
    drop(VdiWriter::create_overlay(&nested, &child, std::slice::from_ref(&base)).unwrap());
    assert!(Image::open(&nested, None).is_err());
    assert!(Image::open_chain(&nested, None, &[]).is_err());
    let disk = Image::open_chain(&nested, None, &[child.clone(), base.clone()]).unwrap();
    assert!(disk.inspection().has_parent);
    let mut actual = [0; 8];
    disk.read_exact_at(5, &mut actual).unwrap();
    assert_eq!(actual, [9; 8]);
    assert!(Image::open_chain(&nested, Some(ImageFormat::Vdi), &[base, child]).is_err());
}

#[test]
fn dispatch_native_embedded_paths_require_explicit_authorization() {
    use virtdisk::{VhdxWriter, VmdkWriter};
    for format in [ImageFormat::Vmdk, ImageFormat::Vhdx] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.img");
        let child = dir.path().join("child.img");
        match format {
            ImageFormat::Vmdk => {
                let w = VmdkWriter::create_sparse(&base, 2 * 1048576).unwrap();
                w.write_all_at(0, &[3; 8]).unwrap();
                w.flush().unwrap();
                drop(w);
                drop(
                    VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base)).unwrap(),
                );
            }
            _ => {
                let w = VhdxWriter::create(&base, 2 * 1048576).unwrap();
                w.write_all_at(0, &[3; 8]).unwrap();
                w.flush().unwrap();
                drop(w);
                drop(VhdxWriter::create_overlay(&child, &base, &[]).unwrap());
            }
        }
        assert!(Image::open(&child, Some(format)).is_err());
        assert!(Image::open_chain(&child, Some(format), &[]).is_err());
        let disk = Image::open_chain(&child, None, std::slice::from_ref(&base)).unwrap();
        assert!(disk.inspection().has_parent);
        let mut bytes = [0; 8];
        disk.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [3; 8]);
    }
}
