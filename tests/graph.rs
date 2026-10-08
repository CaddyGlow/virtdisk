#![cfg(feature = "std")]
use std::path::PathBuf;
use virtdisk::{ImageFormat, ImageGraph, ImageSpec};

#[test]
fn external_snapshot_graph_preserves_branches_and_checks_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let first = directory.path().join("first.qcow2");
    let branch = directory.path().join("branch.qcow2");
    let nested = directory.path().join("nested.qcow2");
    std::fs::write(&base, vec![7; 1024]).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    graph.snapshot(&base, &first).unwrap();
    graph.snapshot(&base, &branch).unwrap();
    graph.snapshot(&first, &nested).unwrap();
    assert_eq!(graph.children(&base).unwrap().len(), 2);
    assert_eq!(
        graph.children(&first).unwrap(),
        vec![nested.canonicalize().unwrap()]
    );
    let mut bytes = [0; 1024];
    graph
        .reader(&nested)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [7; 1024]);
    assert!(graph.delete_snapshot(&base).is_err());
    assert!(graph.delete_snapshot(&first).is_err());
    let flattened = directory.path().join("flat.vdi");
    graph
        .flatten(&nested, &flattened, ImageFormat::Vdi)
        .unwrap();
    let merged = directory.path().join("merged.raw");
    graph
        .merge_to(&nested, &base, &merged, ImageFormat::Raw)
        .unwrap();
    assert_eq!(std::fs::read(merged).unwrap(), vec![7; 1024]);
    assert!(
        graph
            .merge_to(
                &nested,
                &branch,
                directory.path().join("invalid.raw"),
                ImageFormat::Raw
            )
            .is_err()
    );
    graph.delete_snapshot(&nested).unwrap();
    assert!(!nested.exists());
    graph.delete_snapshot(&first).unwrap();
    assert!(branch.exists());
    assert!(base.exists());
}

#[test]
fn graph_checks_declared_parent_identity_cycles_and_foreign_paths() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let other = directory.path().join("other.raw");
    let child = directory.path().join("child.qcow2");
    std::fs::write(&base, vec![1; 512]).unwrap();
    std::fs::write(&other, vec![2; 512]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", 512).unwrap();
    let base_spec = ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    };
    let child_spec = ImageSpec {
        path: child.clone(),
        format: ImageFormat::Qcow2,
        parent: Some(base.clone()),
    };
    assert!(ImageGraph::open(std::slice::from_ref(&child_spec)).is_err());
    assert!(
        ImageGraph::open(&[
            base_spec.clone(),
            ImageSpec {
                parent: Some(other),
                ..child_spec.clone()
            }
        ])
        .is_err()
    );
    let alias = directory.path().join("alias.raw");
    std::fs::hard_link(&base, &alias).unwrap();
    assert!(
        ImageGraph::open(&[
            base_spec.clone(),
            ImageSpec {
                path: alias,
                format: ImageFormat::Raw,
                parent: None
            }
        ])
        .is_err()
    );
    assert!(
        ImageGraph::open(&[
            ImageSpec {
                parent: Some(child.clone()),
                ..base_spec.clone()
            },
            child_spec.clone()
        ])
        .is_err()
    );
    let graph = ImageGraph::open(&[base_spec, child_spec]).unwrap();
    assert!(graph.reader(PathBuf::from("unregistered")).is_err());
    assert_eq!(graph.reader(&child).unwrap().len(), 512);
}

#[test]
fn snapshot_writes_preserve_parent_and_sibling_and_flatten_selected_content() {
    use virtdisk::Qcow2Writer;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let child = directory.path().join("child.qcow2");
    let sibling = directory.path().join("sibling.qcow2");
    std::fs::write(&base, vec![5; 65536]).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    graph.snapshot(&base, &child).unwrap();
    graph.snapshot(&base, &sibling).unwrap();
    {
        let writer = Qcow2Writer::open_chain(&child, std::slice::from_ref(&base)).unwrap();
        writer.write_all_at(100, &[9; 32]).unwrap();
        writer.flush().unwrap();
    }
    let mut expected = vec![5; 65536];
    expected[100..132].fill(9);
    let output = directory.path().join("merged.raw");
    graph
        .merge_to(&child, &base, &output, ImageFormat::Raw)
        .unwrap();
    assert_eq!(std::fs::read(output).unwrap(), expected);
    let mut sibling_bytes = vec![0; 65536];
    graph
        .reader(&sibling)
        .unwrap()
        .read_exact_at(0, &mut sibling_bytes)
        .unwrap();
    assert_eq!(sibling_bytes, vec![5; 65536]);
    assert_eq!(std::fs::read(&base).unwrap(), vec![5; 65536]);
    assert!(graph.snapshot(&base, &child).is_err());
    assert_eq!(graph.children(&base).unwrap().len(), 2);
}

#[test]
fn replaced_registered_file_is_refused_before_management() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    std::fs::write(&base, vec![1; 512]).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let replacement = directory.path().join("replacement.raw");
    std::fs::write(&replacement, vec![2; 512]).unwrap();
    std::fs::rename(&replacement, &base).unwrap();
    assert!(graph.reader(&base).is_err());
    let output = directory.path().join("child.qcow2");
    assert!(graph.snapshot(&base, &output).is_err());
    assert!(!output.exists());
}

#[test]
fn rebase_preserves_selected_bytes_when_new_parent_differs() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let other = directory.path().join("other.raw");
    let child = directory.path().join("child.qcow2");
    let rebased = directory.path().join("rebased.qcow2");
    std::fs::write(&base, vec![3; 65536]).unwrap();
    std::fs::write(&other, vec![8; 65536]).unwrap();
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
    graph.snapshot(&base, &child).unwrap();
    graph.rebase_to(&child, &other, &rebased).unwrap();
    let mut bytes = vec![0; 65536];
    graph
        .reader(&rebased)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, vec![3; 65536]);
    assert_eq!(
        graph.children(&other).unwrap(),
        vec![rebased.canonicalize().unwrap()]
    );
    assert_eq!(std::fs::read(&other).unwrap(), vec![8; 65536]);
    assert_eq!(std::fs::read(&base).unwrap(), vec![3; 65536]);
}

#[test]
fn altered_ancestor_cannot_redirect_reads_to_registered_unrelated_branch() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let evil = directory.path().join("evil.raw");
    let first = directory.path().join("first.qcow2");
    let nested = directory.path().join("nested.qcow2");
    std::fs::write(&base, vec![1; 512]).unwrap();
    std::fs::write(&evil, vec![2; 512]).unwrap();
    let mut graph = ImageGraph::open(&[
        ImageSpec {
            path: base.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: evil.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
    ])
    .unwrap();
    graph.snapshot(&base, &first).unwrap();
    graph.snapshot(&first, &nested).unwrap();
    let mut bytes = std::fs::read(&first).unwrap();
    let redirected = evil.canonicalize().unwrap();
    let name = redirected.to_str().unwrap().as_bytes();
    bytes[128..128 + name.len()].copy_from_slice(name);
    std::fs::write(&first, bytes).unwrap();
    assert!(graph.reader(&nested).is_err());
}

#[test]
fn snapshot_depth_limit_is_checked_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    std::fs::write(&base, vec![1; 512]).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let mut parent = base;
    for index in 0..31 {
        let child = directory.path().join(format!("child{index}.qcow2"));
        graph.snapshot(&parent, &child).unwrap();
        parent = child;
    }
    let output = directory.path().join("too-deep.qcow2");
    assert!(graph.snapshot(&parent, &output).is_err());
    assert!(!output.exists());
}
