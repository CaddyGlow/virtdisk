use virtdisk::{GraphManifest, ImageFormat, ImageGraph, ImageSpec};

#[test]
fn manifest_round_trip_restores_branches_selection_and_requires_exact_authority() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.raw");
    let first = dir.path().join("first.qcow2");
    let branch = dir.path().join("branch.qcow2");
    let nested = dir.path().join("nested.qcow2");
    std::fs::write(&base, vec![37; 1024]).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    graph.snapshot(&base, &first).unwrap();
    graph.snapshot(&base, &branch).unwrap();
    graph.snapshot(&first, &nested).unwrap();
    let output = dir.path().join("graph.manifest");
    graph
        .manifest(Some(&nested))
        .unwrap()
        .save(&output)
        .unwrap();
    drop(graph);
    let manifest = GraphManifest::open(&output).unwrap();
    assert_eq!(
        manifest.selected(),
        Some(nested.canonicalize().unwrap().as_path())
    );
    assert_eq!(manifest.images().len(), 4);
    let paths = [base.clone(), first.clone(), branch.clone(), nested.clone()];
    assert!(manifest.open_graph(&paths[..3]).is_err());
    assert!(
        manifest
            .open_graph(&[base.clone(), base.clone(), branch.clone(), nested.clone()])
            .is_err()
    );
    let foreign = dir.path().join("foreign.raw");
    std::fs::write(&foreign, [0; 512]).unwrap();
    let mut extra = paths.to_vec();
    extra.push(foreign);
    assert!(manifest.open_graph(&extra).is_err());
    let mut reopened = manifest.open_graph(&paths).unwrap();
    assert_eq!(reopened.children(&base).unwrap().len(), 2);
    let mut bytes = [0; 1024];
    reopened
        .reader(manifest.selected().unwrap())
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [37; 1024]);
    assert!(reopened.delete_snapshot(&first).is_err());
    reopened.delete_snapshot(&nested).unwrap();
    assert!(
        GraphManifest::open(&output)
            .unwrap()
            .open_graph(&paths)
            .is_err()
    );
}

#[test]
fn saving_is_no_overwrite_and_loading_detects_corruption_and_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    std::fs::write(&base, [37; 512]).unwrap();
    let graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    assert!(graph.manifest(Some(&dir.path().join("missing"))).is_err());
    let manifest = graph.manifest(None).unwrap();
    let output = dir.path().join("manifest");
    manifest.save(&output).unwrap();
    let original = std::fs::read(&output).unwrap();
    assert_eq!(
        manifest.save(&output).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(&output).unwrap(), original);
    for length in [0, 7, 15, original.len() - 1] {
        std::fs::write(&output, &original[..length]).unwrap();
        assert!(GraphManifest::open(&output).is_err());
    }
    let mut corrupted = original.clone();
    corrupted[16] ^= 1;
    std::fs::write(&output, corrupted).unwrap();
    assert!(GraphManifest::open(&output).is_err());
    let mut extra = original;
    extra.push(0);
    std::fs::write(&output, extra).unwrap();
    assert!(GraphManifest::open(&output).is_err());
}

#[test]
fn empty_graph_has_no_selection_and_manifest_size_is_bounded_before_reading() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("manifest");
    ImageGraph::open(&[])
        .unwrap()
        .manifest(None)
        .unwrap()
        .save(&output)
        .unwrap();
    let manifest = GraphManifest::open(&output).unwrap();
    assert!(manifest.selected().is_none());
    assert!(manifest.images().is_empty());
    manifest.open_graph(&[]).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&output)
        .unwrap()
        .set_len(1048577)
        .unwrap();
    assert!(GraphManifest::open(&output).is_err());
}

fn encoded_manifest(version: u32, selected: u16, records: &[(u8, u8, u16, Vec<u8>)]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut bytes = b"VDGRAPH\0".to_vec();
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&(records.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&selected.to_le_bytes());
    for (format, codec, parent, path) in records {
        bytes.extend_from_slice(&[*format, *codec]);
        bytes.extend_from_slice(&parent.to_le_bytes());
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path);
    }
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}

#[test]
fn independently_encoded_manifest_is_parsed_without_opening_embedded_paths() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("does-not-exist");
    let granted = dir.path().join("granted");
    std::fs::write(&granted, [0; 512]).unwrap();
    let output = dir.path().join("manifest");
    std::fs::write(
        &output,
        encoded_manifest(
            1,
            0,
            &[(0, 0, u16::MAX, absent.to_str().unwrap().as_bytes().to_vec())],
        ),
    )
    .unwrap();
    let manifest = GraphManifest::open(output).unwrap();
    assert_eq!(manifest.selected(), Some(absent.as_path()));
    assert_eq!(manifest.images()[0].format, ImageFormat::Raw);
    assert_eq!(
        manifest.open_graph(&[granted]).err().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

#[test]
fn checksummed_invalid_topology_tags_and_path_bounds_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("manifest");
    let a = dir.path().join("a").to_str().unwrap().as_bytes().to_vec();
    let b = dir.path().join("b").to_str().unwrap().as_bytes().to_vec();
    let cases = [
        (1, 0, vec![]),
        (2, u16::MAX, vec![]),
        (1, u16::MAX, vec![(99, 0, u16::MAX, a.clone())]),
        (1, u16::MAX, vec![(0, 99, u16::MAX, a.clone())]),
        (1, u16::MAX, vec![(0, 0, 1, a.clone())]),
        (1, u16::MAX, vec![(1, 0, 0, a.clone())]),
        (
            1,
            u16::MAX,
            vec![(1, 0, 1, a.clone()), (1, 0, 0, b.clone())],
        ),
        (
            1,
            u16::MAX,
            vec![(0, 0, u16::MAX, a.clone()), (0, 0, u16::MAX, a.clone())],
        ),
        (
            1,
            u16::MAX,
            vec![(0, 0, 1, a.clone()), (1, 0, u16::MAX, b.clone())],
        ),
        (1, u16::MAX, vec![(3, 0, 1, a.clone()), (0, 0, u16::MAX, b)]),
        (1, u16::MAX, vec![(0, 0, u16::MAX, b"relative".to_vec())]),
        (1, u16::MAX, vec![(0, 0, u16::MAX, vec![])]),
        (1, u16::MAX, vec![(0, 0, u16::MAX, vec![b'a'; 65537])]),
        (1, u16::MAX, vec![(0, 0, u16::MAX, vec![0xff])]),
        (1, u16::MAX, vec![(0, 0, u16::MAX, b"/a\0b".to_vec())]),
    ];
    for (version, selected, records) in cases {
        std::fs::write(&output, encoded_manifest(version, selected, &records)).unwrap();
        assert!(GraphManifest::open(&output).is_err());
    }
    let records: Vec<_> = (0..33)
        .map(|i| {
            (
                1,
                0,
                if i == 0 { u16::MAX } else { i - 1 },
                dir.path()
                    .join(format!("node{i}"))
                    .to_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            )
        })
        .collect();
    std::fs::write(&output, encoded_manifest(1, u16::MAX, &records)).unwrap();
    assert!(GraphManifest::open(&output).is_err());
    let records: Vec<_> = (0..129)
        .map(|i| {
            (
                0,
                0,
                u16::MAX,
                dir.path()
                    .join(format!("node{i}"))
                    .to_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            )
        })
        .collect();
    std::fs::write(&output, encoded_manifest(1, u16::MAX, &records)).unwrap();
    assert!(GraphManifest::open(&output).is_err());
}

#[test]
fn native_families_and_live_parent_edges_are_revalidated_after_reload() {
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let child = dir.path().join("child");
        drop(virtdisk::ImageWriter::create(&base, format, 1024).unwrap());
        let mut graph = ImageGraph::open(&[ImageSpec {
            path: base.clone(),
            format,
            parent: None,
        }])
        .unwrap();
        graph.snapshot_as(&base, &child, format).unwrap();
        let output = dir.path().join("manifest");
        graph.manifest(Some(&child)).unwrap().save(&output).unwrap();
        let original_base = std::fs::read(&base).unwrap();
        drop(graph);
        let manifest = GraphManifest::open(&output).unwrap();
        let reopened = manifest.open_graph(&[child.clone(), base.clone()]).unwrap();
        let mut bytes = [37; 1024];
        reopened
            .reader(&child)
            .unwrap()
            .read_exact_at(0, &mut bytes)
            .unwrap();
        assert_eq!(bytes, [0; 1024]);
        assert_eq!(std::fs::read(&base).unwrap(), original_base);
        std::fs::write(&child, [0; 1024]).unwrap();
        assert!(manifest.open_graph(&[base, child]).is_err());
    }
}

#[cfg(unix)]
#[test]
fn non_unicode_unix_paths_round_trip_losslessly() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let base = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"base-\xff".to_vec()));
    std::fs::write(&base, [37; 512]).unwrap();
    let graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let output = dir.path().join("manifest");
    graph.manifest(Some(&base)).unwrap().save(&output).unwrap();
    let manifest = GraphManifest::open(&output).unwrap();
    assert_eq!(
        manifest.selected(),
        Some(base.canonicalize().unwrap().as_path())
    );
    let graph = manifest.open_graph(std::slice::from_ref(&base)).unwrap();
    let mut bytes = [0; 512];
    graph
        .reader(base)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [37; 512]);
}

#[test]
fn independently_encoded_maximum_node_and_depth_bounds_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("manifest");
    let records: Vec<_> = (0..128)
        .map(|i| {
            (
                0,
                0,
                u16::MAX,
                dir.path()
                    .join(format!("node{i}"))
                    .to_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            )
        })
        .collect();
    std::fs::write(&output, encoded_manifest(1, 127, &records)).unwrap();
    let parsed = GraphManifest::open(&output).unwrap();
    assert_eq!(parsed.images().len(), 128);
    assert_eq!(
        parsed.selected(),
        Some(dir.path().join("node127").as_path())
    );
    let records: Vec<_> = (0..32)
        .map(|i| {
            (
                1,
                0,
                if i == 0 { u16::MAX } else { i - 1 },
                dir.path()
                    .join(format!("node{i}"))
                    .to_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            )
        })
        .collect();
    std::fs::write(&output, encoded_manifest(1, 31, &records)).unwrap();
    assert_eq!(GraphManifest::open(&output).unwrap().images().len(), 32);
}
