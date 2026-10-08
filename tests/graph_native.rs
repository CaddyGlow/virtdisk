#![cfg(feature = "std")]
use virtdisk::{ImageFormat, ImageGraph, ImageSpec, VdiWriter};

#[test]
#[cfg(not(target_os = "linux"))]
fn unsupported_vmdk_snapshot_preserves_registration_and_parent() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.vmdk");
    let child = directory.path().join("child.vmdk");
    drop(virtdisk::VmdkWriter::create(&base, 65536).unwrap());
    let before = std::fs::read(&base).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Vmdk,
        parent: None,
    }])
    .unwrap();
    assert_eq!(
        graph
            .snapshot_as(&base, &child, ImageFormat::Vmdk)
            .unwrap_err()
            .kind(),
        virtdisk::io::ErrorKind::Unsupported
    );
    assert!(!child.exists());
    assert_eq!(graph.images().len(), 1);
    assert_eq!(std::fs::read(&base).unwrap(), before);
}
#[test]
fn native_snapshot_branches_flatten_and_preserve_immutable_base() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vdi");
    let child = dir.path().join("child.vdi");
    let sibling = dir.path().join("sibling.vdi");
    let nested = dir.path().join("nested.vdi");
    let flat = dir.path().join("flat.raw");
    let writer = VdiWriter::create(&base, 1048576).unwrap();
    writer.write_all_at(4, &[7; 8]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let original = std::fs::read(&base).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Vdi,
        parent: None,
    }])
    .unwrap();
    graph.snapshot_as(&base, &child, ImageFormat::Vdi).unwrap();
    graph
        .snapshot_as(&base, &sibling, ImageFormat::Vdi)
        .unwrap();
    graph
        .snapshot_as(&child, &nested, ImageFormat::Vdi)
        .unwrap();
    let writer = VdiWriter::open_chain(&nested, &[child.clone(), base.clone()]).unwrap();
    if !cfg!(target_os = "linux") {
        assert_eq!(
            writer.write_all_at(5, &[9]).unwrap_err().kind(),
            virtdisk::io::ErrorKind::Unsupported
        );
        drop(writer);
        let mut bytes = [0; 8];
        graph
            .reader(&nested)
            .unwrap()
            .read_exact_at(4, &mut bytes)
            .unwrap();
        assert_eq!(bytes, [7; 8]);
        assert_eq!(std::fs::read(&base).unwrap(), original);
        return;
    }
    writer.write_all_at(5, &[9]).unwrap();
    writer.flush().unwrap();
    assert!(graph.delete_snapshot(&nested).is_err());
    drop(writer);
    let mut actual = [0; 8];
    graph
        .reader(&nested)
        .unwrap()
        .read_exact_at(4, &mut actual)
        .unwrap();
    assert_eq!(actual, [7, 9, 7, 7, 7, 7, 7, 7]);
    graph
        .reader(&sibling)
        .unwrap()
        .read_exact_at(4, &mut actual)
        .unwrap();
    assert_eq!(actual, [7; 8]);
    graph.flatten(&nested, &flat, ImageFormat::Raw).unwrap();
    assert_eq!(std::fs::read(&base).unwrap(), original);
    assert!(graph.delete_snapshot(&child).is_err());
    graph.delete_snapshot(&nested).unwrap();
}

#[test]
fn hosted_vmdk_and_vhdx_snapshot_branches_and_stale_parent_epochs() {
    use virtdisk::{VhdxWriter, VmdkWriter};
    for format in [
        #[cfg(target_os = "linux")]
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.img");
        let child = dir.path().join("child.img");
        let sibling = dir.path().join("sibling.img");
        let nested = dir.path().join("nested.img");
        match format {
            ImageFormat::Vmdk => {
                let w = VmdkWriter::create_sparse(&base, 2 * 1048576).unwrap();
                w.write_all_at(0, &[7; 16]).unwrap();
                w.flush().unwrap();
            }
            _ => {
                let w = VhdxWriter::create(&base, 2 * 1048576).unwrap();
                w.write_all_at(0, &[7; 16]).unwrap();
                w.flush().unwrap();
            }
        }
        let original = std::fs::read(&base).unwrap();
        let mut graph = ImageGraph::open(&[ImageSpec {
            path: base.clone(),
            format,
            parent: None,
        }])
        .unwrap();
        graph.snapshot_as(&base, &child, format).unwrap();
        graph.snapshot_as(&base, &sibling, format).unwrap();
        graph.snapshot_as(&child, &nested, format).unwrap();
        let authorities = [child.clone(), base.clone()];
        match format {
            ImageFormat::Vmdk => {
                let w = VmdkWriter::open_chain(&nested, &authorities).unwrap();
                w.write_zeroes(4, 2).unwrap();
                w.flush().unwrap();
            }
            _ => {
                let w = VhdxWriter::open_chain(&nested, &authorities).unwrap();
                w.write_zeroes(4, 2).unwrap();
                w.flush().unwrap();
            }
        }
        let mut bytes = [0; 16];
        graph
            .reader(&nested)
            .unwrap()
            .read_exact_at(0, &mut bytes)
            .unwrap();
        let mut expected = [7; 16];
        expected[4..6].fill(0);
        assert_eq!(bytes, expected);
        graph
            .reader(&sibling)
            .unwrap()
            .read_exact_at(0, &mut bytes)
            .unwrap();
        assert_eq!(bytes, [7; 16]);
        assert_eq!(std::fs::read(&base).unwrap(), original);
        match format {
            ImageFormat::Vmdk => {
                let w = VmdkWriter::open(&base).unwrap();
                w.write_all_at(0, &[9]).unwrap();
                w.flush().unwrap();
            }
            _ => {
                let w = VhdxWriter::open(&base).unwrap();
                w.write_all_at(0, &[9]).unwrap();
                w.flush().unwrap();
            }
        }
        assert!(graph.reader(&nested).is_err());
        assert!(graph.delete_snapshot(&sibling).is_err());
    }
}

#[test]
fn native_snapshot_rejects_cross_family_and_overwrite_before_publication() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vdi");
    let output = dir.path().join("existing");
    drop(VdiWriter::create(&base, 1048576).unwrap());
    std::fs::write(&output, b"keep").unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Vdi,
        parent: None,
    }])
    .unwrap();
    assert!(
        graph
            .snapshot_as(&base, &output, ImageFormat::Vhdx)
            .is_err()
    );
    assert!(graph.snapshot_as(&base, &output, ImageFormat::Vdi).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"keep");
}
