#![cfg(feature = "std")]
use std::path::Path;
use virtdisk::io::{Seek, SeekFrom, Write};
use virtdisk::{ImageFormat, ImageGraph, ImageSpec};

fn change_parent(child: &Path, parent: &Path, format: &str) {
    let parent = parent.canonicalize().unwrap();
    let name = parent.to_str().unwrap();
    let mut file = std::fs::OpenOptions::new().write(true).open(child).unwrap();
    file.seek(SeekFrom::Start(16)).unwrap();
    file.write_all(&(name.len() as u32).to_be_bytes()).unwrap();
    file.seek(SeekFrom::Start(108)).unwrap();
    file.write_all(&(format.len() as u32).to_be_bytes())
        .unwrap();
    file.write_all(format.as_bytes()).unwrap();
    file.seek(SeekFrom::Start(128)).unwrap();
    file.write_all(name.as_bytes()).unwrap();
}

#[test]
fn descendant_reader_rejects_ancestor_redirected_to_registered_other_branch() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.raw");
    let other = dir.path().join("other.raw");
    let parent = dir.path().join("parent.qcow2");
    let child = dir.path().join("child.qcow2");
    std::fs::write(&base, vec![1; 65536]).unwrap();
    std::fs::write(&other, vec![2; 65536]).unwrap();
    let mut graph = ImageGraph::open(&[
        ImageSpec {
            path: base.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: other.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
    ])
    .unwrap();
    graph.snapshot(&base, &parent).unwrap();
    graph.snapshot(&parent, &child).unwrap();
    change_parent(&parent, &other, "raw");
    assert!(
        graph.reader(&child).is_err(),
        "in-place parent redirect bypassed graph ancestry"
    );
}

#[test]
fn deleting_leaf_revalidates_registered_branch_edges_before_removal() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.raw");
    let leaf = dir.path().join("leaf.qcow2");
    let branch = dir.path().join("branch.qcow2");
    std::fs::write(&base, vec![1; 65536]).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    graph.snapshot(&base, &leaf).unwrap();
    graph.snapshot(&base, &branch).unwrap();
    change_parent(&branch, &leaf, "qcow2");
    assert!(
        graph.delete_snapshot(&leaf).is_err(),
        "stale graph deleted an actual registered parent"
    );
    assert!(leaf.exists());
}

#[test]
fn deleting_snapshot_refuses_active_cooperating_writer() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let child = directory.path().join("child.qcow2");
    std::fs::write(&base, vec![1; 512]).unwrap();
    let mut graph = virtdisk::ImageGraph::open(&[virtdisk::ImageSpec {
        path: base.clone(),
        format: virtdisk::ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    graph.snapshot(&base, &child).unwrap();
    let writer = virtdisk::Qcow2Writer::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    assert!(graph.delete_snapshot(&child).is_err());
    assert!(child.exists());
    drop(writer);
    graph.delete_snapshot(&child).unwrap();
    assert!(!child.exists());
}
