#![cfg(feature = "std")]
use std::path::Path;
use virtdisk::io::{Seek, SeekFrom, Write};
use virtdisk::{ImageFormat, ImageGraph, ImageSpec, ParserLimits, Qcow2, ReadAt};

fn remove_backing_format(child: &Path) {
    let mut file = std::fs::OpenOptions::new().write(true).open(child).unwrap();
    file.seek(SeekFrom::Start(104)).unwrap();
    // Replace the format extension and padding with the native end marker.
    // The filename remains at offset 128, and no allocation ownership changes.
    file.write_all(&[0; 24]).unwrap();
    file.sync_all().unwrap();
}
fn qcow_parent(path: &Path) {
    let writer = virtdisk::ImageWriter::create(path, ImageFormat::Qcow2, 65536).unwrap();
    virtdisk::WriteAt::write_all_at(&writer, 0, &[37; 65536]).unwrap();
    virtdisk::WriteAt::flush(&writer).unwrap();
}

#[test]
fn graph_refuses_a_raw_declaration_when_unspecified_backing_resolves_to_qcow2() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    qcow_parent(&base);
    let capacity = std::fs::metadata(&base).unwrap().len();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", capacity).unwrap();
    remove_backing_format(&child);
    let specs = [
        ImageSpec {
            path: base.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: child.clone(),
            format: ImageFormat::Qcow2,
            parent: Some(base.clone()),
        },
    ];
    // The native reader legitimately detects QCOW2 when no type was supplied.
    let native = Qcow2::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    let mut bytes = [0; 4];
    native.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 4]);
    assert_eq!(&std::fs::read(&base).unwrap()[..4], b"QFI\xfb");
    assert!(
        ImageGraph::open(&specs).is_err(),
        "graph accepted a reader whose backing interpretation differs from its raw declaration"
    );
    assert!(ImageGraph::open_with_limits(&specs, ParserLimits::default()).is_err());
}

#[test]
fn graph_accepts_unspecified_types_when_detected_family_matches_the_declaration() {
    for format in [ImageFormat::Raw, ImageFormat::Qcow2] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let child = dir.path().join("child");
        if format == ImageFormat::Raw {
            std::fs::write(&base, [37; 65536]).unwrap();
        } else {
            qcow_parent(&base);
        }
        let label = if format == ImageFormat::Raw {
            "raw"
        } else {
            "qcow2"
        };
        virtdisk::create_qcow2_overlay(&child, &base, label, 65536).unwrap();
        remove_backing_format(&child);
        let specs = [
            ImageSpec {
                path: base.clone(),
                format,
                parent: None,
            },
            ImageSpec {
                path: child.clone(),
                format: ImageFormat::Qcow2,
                parent: Some(base),
            },
        ];
        let graph = ImageGraph::open_with_limits(&specs, ParserLimits::default()).unwrap();
        let mut bytes = [0; 65536];
        graph
            .reader(child)
            .unwrap()
            .read_exact_at(0, &mut bytes)
            .unwrap();
        assert_eq!(bytes, [37; 65536]);
    }
}

#[test]
fn an_explicit_raw_extension_preserves_opaque_qcow2_bytes_and_deeper_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    let nested = dir.path().join("nested");
    qcow_parent(&base);
    let expected = std::fs::read(&base).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", expected.len() as u64).unwrap();
    let specs = [
        ImageSpec {
            path: base.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: child.clone(),
            format: ImageFormat::Qcow2,
            parent: Some(base.clone()),
        },
    ];
    let mut graph = ImageGraph::open_with_limits(&specs, ParserLimits::default()).unwrap();
    graph.snapshot(&child, &nested).unwrap();
    let mut bytes = vec![0; expected.len()];
    graph
        .reader(nested)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(std::fs::read(base).unwrap(), expected);
}

#[test]
fn manifest_binding_rejects_a_type_disagreement_despite_exact_path_authority() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    qcow_parent(&base);
    let capacity = std::fs::metadata(&base).unwrap().len();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", capacity).unwrap();
    remove_backing_format(&child);
    let mut bytes = b"VDGRAPH\0".to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    for (path, format, parent) in [(&base, 0, u16::MAX), (&child, 1, 0)] {
        let canonical = path.canonicalize().unwrap();
        let name = canonical.to_str().unwrap().as_bytes();
        bytes.extend_from_slice(&[format, 0]);
        bytes.extend_from_slice(&parent.to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
        bytes.extend_from_slice(name);
    }
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    let manifest_path = dir.path().join("manifest");
    std::fs::write(&manifest_path, bytes).unwrap();
    let manifest = virtdisk::GraphManifest::open(manifest_path).unwrap();
    let paths = [base, child];
    assert_eq!(
        manifest.open_graph(&paths).err().unwrap().kind(),
        virtdisk::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        manifest
            .open_graph_with_limits(&paths, ParserLimits::default())
            .err()
            .unwrap()
            .kind(),
        virtdisk::io::ErrorKind::InvalidInput
    );
}

#[test]
fn an_existing_graph_rechecks_type_before_returning_readers_or_deleting_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    let output = dir.path().join("snapshot");
    qcow_parent(&base);
    let original = std::fs::read(&base).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", original.len() as u64).unwrap();
    let specs = [
        ImageSpec {
            path: base.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: child.clone(),
            format: ImageFormat::Qcow2,
            parent: Some(base.clone()),
        },
    ];
    let mut graph = ImageGraph::open_with_limits(&specs, ParserLimits::default()).unwrap();
    let manifest = graph.manifest(Some(&child)).unwrap();
    remove_backing_format(&child);
    let changed_child = std::fs::read(&child).unwrap();
    assert!(graph.reader(&child).is_err());
    assert!(graph.snapshot(&child, &output).is_err());
    assert!(graph.delete_snapshot(&child).is_err());
    assert!(manifest.open_graph(&[base.clone(), child.clone()]).is_err());
    assert!(!output.exists());
    assert_eq!(
        graph.children(&base).unwrap(),
        [child.canonicalize().unwrap()]
    );
    assert_eq!(std::fs::read(&child).unwrap(), changed_child);
    assert_eq!(std::fs::read(base).unwrap(), original);
}

#[test]
#[ignore = "requires independent qemu-img backing interpretation oracle"]
fn qemu_agrees_with_explicit_raw_and_implicit_qcow2_interpretations() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    let raw = dir.path().join("oracle.raw");
    qcow_parent(&base);
    let opaque = std::fs::read(&base).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", opaque.len() as u64).unwrap();
    let convert = || {
        let result = Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&child)
            .arg(&raw)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::read(&raw).unwrap()
    };
    assert_eq!(convert(), opaque);
    std::fs::remove_file(&raw).unwrap();
    remove_backing_format(&child);
    let mut decoded = vec![0; opaque.len()];
    decoded[..65536].fill(37);
    assert_eq!(convert(), decoded);
    std::fs::remove_file(&raw).unwrap();
    make_v2_without_format(&child);
    assert_eq!(convert(), decoded);
    let specs = [
        ImageSpec {
            path: base.clone(),
            format: ImageFormat::Qcow2,
            parent: None,
        },
        ImageSpec {
            path: child.clone(),
            format: ImageFormat::Qcow2,
            parent: Some(base.clone()),
        },
    ];
    let graph = ImageGraph::open_with_limits(&specs, ParserLimits::default()).unwrap();
    let mut bytes = vec![0; opaque.len()];
    graph
        .reader(child)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, decoded);
    assert_eq!(std::fs::read(base).unwrap(), opaque);
}

fn make_v2_without_format(child: &Path) {
    let mut file = std::fs::OpenOptions::new().write(true).open(child).unwrap();
    file.seek(SeekFrom::Start(4)).unwrap();
    file.write_all(&2u32.to_be_bytes()).unwrap();
    file.seek(SeekFrom::Start(72)).unwrap();
    file.write_all(&[0; 56]).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn v2_chains_check_resolved_types_and_preserve_matching_qcow2_parents() {
    for format in [ImageFormat::Raw, ImageFormat::Qcow2] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let child = dir.path().join("child");
        qcow_parent(&base);
        virtdisk::create_qcow2_overlay(&child, &base, "qcow2", 65536).unwrap();
        make_v2_without_format(&child);
        let specs = [
            ImageSpec {
                path: base.clone(),
                format,
                parent: None,
            },
            ImageSpec {
                path: child.clone(),
                format: ImageFormat::Qcow2,
                parent: Some(base),
            },
        ];
        let result = ImageGraph::open_with_limits(&specs, ParserLimits::default());
        if format == ImageFormat::Raw {
            assert!(result.is_err());
        } else {
            let mut bytes = [0; 512];
            result
                .unwrap()
                .reader(child)
                .unwrap()
                .read_exact_at(0, &mut bytes)
                .unwrap();
            assert_eq!(bytes, [37; 512]);
        }
    }
}
