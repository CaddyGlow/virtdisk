#![cfg(all(feature = "cli", target_os = "linux"))]
use std::process::Command;
use virtdisk::{GraphManifest, ImageFormat, ImageGraph, ImageSpec, ReadAt};

#[test]
fn graph_snapshot_publishes_selected_generation_with_explicit_authority() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let declaration = dir.path().join("input.manifest");
    let output = dir.path().join("generation");
    std::fs::write(&base, [37; 512]).unwrap();
    ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap()
    .manifest(Some(&base))
    .unwrap()
    .save(&declaration)
    .unwrap();
    let original = std::fs::read(&declaration).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "graph", "snapshot"])
        .arg(&declaration)
        .arg(&base)
        .arg(&output)
        .arg("qcow2")
        .arg(&base)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("generation-publication"));
    let loaded = GraphManifest::open(output.join("graph.manifest")).unwrap();
    let image = output.join("image");
    assert_eq!(loaded.selected(), Some(image.as_path()));
    let reopened = loaded.open_graph(&[base.clone(), image.clone()]).unwrap();
    let mut bytes = [0; 512];
    reopened
        .reader(&image)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [37; 512]);
    assert_eq!(std::fs::read(declaration).unwrap(), original);
}

#[test]
fn graph_snapshot_requires_authority_and_honors_payload_quota() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let declaration = dir.path().join("input.manifest");
    std::fs::write(&base, [37; 512]).unwrap();
    ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap()
    .manifest(None)
    .unwrap()
    .save(&declaration)
    .unwrap();
    for authorized in [false, true] {
        let output = dir
            .path()
            .join(if authorized { "quota" } else { "unauthorized" });
        let mut command = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        command.arg("--json-errors");
        if authorized {
            command.args(["--operation-limit", "bytes=0"]);
        }
        command
            .args(["graph", "snapshot"])
            .arg(&declaration)
            .arg(&base)
            .arg(&output)
            .arg("qcow2");
        if authorized {
            command.arg(&base);
        }
        let result = command.output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(!output.exists());
        if authorized {
            assert!(String::from_utf8_lossy(&result.stderr).contains("resource-limit"));
        }
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn graph_snapshot_rejects_recovery_controls_before_manifest_io() {
    for controls in [vec!["--recover"], vec!["--replay-vhdx-log"]] {
        let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .arg("--json-errors")
            .args(controls)
            .args([
                "graph",
                "snapshot",
                "missing.manifest",
                "missing.parent",
                "generation",
                "qcow2",
            ])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&result.stderr).contains("invalid-input"));
    }
}

#[test]
fn graph_materialization_commands_preserve_sources_and_enforce_ancestry() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let other = dir.path().join("other");
    let child = dir.path().join("child");
    let declaration = dir.path().join("input.manifest");
    std::fs::write(&base, [37; 512]).unwrap();
    std::fs::write(&other, [91; 512]).unwrap();
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
    graph
        .manifest(Some(&child))
        .unwrap()
        .save(&declaration)
        .unwrap();
    let original = std::fs::read(&child).unwrap();
    for action in ["flatten", "merge", "rebase"] {
        let output = dir.path().join(action);
        let mut command = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        command
            .args(["--progress", "graph", action])
            .arg(&declaration);
        command.arg(if action == "rebase" { &other } else { &child });
        if action != "flatten" {
            command.arg(&base);
        }
        command.arg(&output);
        if action != "rebase" {
            command.arg("raw");
        }
        command.args([&base, &other, &child]);
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        for (flag, limit, code) in [
            ("--operation-limit", "io=0", "resource-limit"),
            ("--parser-limit", "metadata=1", "parser-limit"),
        ] {
            let refused = dir.path().join(format!("{action}-{limit}"));
            let mut command = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
            command
                .args(["--json-errors", flag, limit, "graph", action])
                .arg(&declaration);
            command.arg(if action == "rebase" { &other } else { &child });
            if action != "flatten" {
                command.arg(&base);
            }
            command.arg(&refused);
            if action != "rebase" {
                command.arg("raw");
            }
            command.args([&base, &other, &child]);
            let result = command.output().unwrap();
            assert_eq!(result.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&result.stderr).contains(code));
            assert!(!refused.exists());
        }
        let reader = if action == "rebase" {
            virtdisk::Image::open_chain(
                &output,
                Some(ImageFormat::Qcow2),
                std::slice::from_ref(&base),
            )
        } else {
            virtdisk::Image::open(&output, Some(ImageFormat::Raw))
        }
        .unwrap();
        let mut bytes = [0; 512];
        reader.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [if action == "rebase" { 91 } else { 37 }; 512]);
    }
    let refused = dir.path().join("refused");
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["graph", "merge"])
        .arg(&declaration)
        .arg(&child)
        .arg(&other)
        .arg(&refused)
        .arg("raw")
        .args([&base, &other, &child])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(!refused.exists());
    assert_eq!(std::fs::read(child).unwrap(), original);
    assert_eq!(std::fs::read(base).unwrap(), [37; 512]);
    assert_eq!(std::fs::read(other).unwrap(), [91; 512]);
}

#[test]
fn graph_rebase_generation_publishes_image_with_selected_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    let source = dir.path().join("source");
    let input = dir.path().join("input.manifest");
    let output = dir.path().join("generation");
    std::fs::write(&parent, [37; 512]).unwrap();
    std::fs::write(&source, [91; 512]).unwrap();
    ImageGraph::open(&[
        ImageSpec {
            path: parent.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: source.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
    ])
    .unwrap()
    .manifest(None)
    .unwrap()
    .save(&input)
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "graph", "rebase-generation"])
        .arg(&input)
        .arg(&source)
        .arg(&parent)
        .arg(&output)
        .args([&source, &parent])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("generation-publication"));
    let image = output.join("image");
    let manifest = GraphManifest::open(output.join("graph.manifest")).unwrap();
    assert_eq!(manifest.selected(), Some(image.as_path()));
    let graph = manifest
        .open_graph(&[source.clone(), parent.clone(), image.clone()])
        .unwrap();
    let mut bytes = [0; 512];
    graph
        .reader(&image)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [91; 512]);
}

#[test]
fn graph_selection_persists_registered_state_without_changing_branches() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let other = dir.path().join("other");
    let input = dir.path().join("input.manifest");
    let selected = dir.path().join("selected.manifest");
    std::fs::write(&base, [37; 512]).unwrap();
    std::fs::write(&other, [91; 512]).unwrap();
    ImageGraph::open(&[
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
    .unwrap()
    .manifest(Some(&base))
    .unwrap()
    .save(&input)
    .unwrap();
    let original = std::fs::read(&input).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "--operation-limit", "io=0", "graph", "select"])
        .arg(&input)
        .arg(&other)
        .arg(&selected)
        .args([&base, &other])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let progress = String::from_utf8_lossy(&result.stderr);
    assert!(progress.contains("metadata-validation"));
    assert!(progress.contains("publication"));
    let manifest = GraphManifest::open(&selected).unwrap();
    assert_eq!(manifest.selected(), Some(other.as_path()));
    let graph = manifest.open_graph(&[base.clone(), other.clone()]).unwrap();
    let mut bytes = [0; 512];
    graph
        .reader(manifest.selected().unwrap())
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [91; 512]);
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert_eq!(std::fs::read(&base).unwrap(), [37; 512]);
    assert_eq!(std::fs::read(&other).unwrap(), [91; 512]);
    let before = std::fs::read(&selected).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["graph", "select"])
        .arg(&input)
        .arg(&base)
        .arg(&selected)
        .args([&base, &other])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(std::fs::read(&selected).unwrap(), before);
}

#[test]
fn graph_info_reports_selected_state_edges_sizes_and_escaped_paths() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base\"line\n");
    let child = dir.path().join("child");
    let manifest = dir.path().join("graph.manifest");
    std::fs::write(&base, [37; 512]).unwrap();
    let mut graph = ImageGraph::open(&[ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    graph.snapshot(&base, &child).unwrap();
    graph
        .manifest(Some(&child))
        .unwrap()
        .save(&manifest)
        .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["graph", "info"])
        .arg(&manifest)
        .args([&base, &child])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let record = String::from_utf8(result.stdout).unwrap();
    assert!(record.contains("\"selected\":1"));
    assert!(record.contains("\"parent\":0,\"virtual_size\":512"));
    assert!(record.contains("\"parent\":null,\"virtual_size\":512"));
    assert!(record.contains("base\\\"line\\n"));
    assert!(record.contains("\"path_encoding\":\"unix-bytes\""));
    for limits in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        if limits {
            command.args(["--parser-limit", "metadata=1"]);
        }
        command.args(["graph", "info"]).arg(&manifest).arg(&base);
        if limits {
            command.arg(&child);
        }
        let result = command.output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
    }
    assert_eq!(std::fs::read(base).unwrap(), [37; 512]);
}

#[test]
fn graph_info_preserves_non_unicode_path_bytes_and_rejects_log_replay() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let image = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"image\xff".to_vec()));
    let manifest = dir.path().join("graph.manifest");
    std::fs::write(&image, [37; 512]).unwrap();
    ImageGraph::open(&[ImageSpec {
        path: image.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap()
    .manifest(None)
    .unwrap()
    .save(&manifest)
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["graph", "info"])
        .arg(&manifest)
        .arg(&image)
        .output()
        .unwrap();
    assert!(result.status.success());
    let record = String::from_utf8(result.stdout).unwrap();
    assert!(record.contains("\"selected\":null"));
    assert!(record.contains("\"path_display\":null"));
    assert!(record.contains("696d616765ff\""));
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args([
            "--json-errors",
            "--replay-vhdx-log",
            "graph",
            "info",
            "missing.manifest",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid-input"));
    assert!(result.stdout.is_empty());
}
