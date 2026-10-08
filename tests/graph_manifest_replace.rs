#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use std::ops::ControlFlow;
use virtdisk::io;
use virtdisk::{
    GraphManifest, ImageFormat, ImageGraph, ImageSpec, OperationContext, OperationPhase,
    OperationProgress,
};

#[test]
fn atomic_selection_replacement_refuses_stale_declarations_and_preserves_images() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    std::fs::write(&first, [37; 512]).unwrap();
    std::fs::write(&second, [9; 512]).unwrap();
    let graph = ImageGraph::open(&[
        ImageSpec {
            path: first.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: second.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
    ])
    .unwrap();
    let expected = graph.manifest(Some(&first)).unwrap();
    let next = graph.manifest(Some(&second)).unwrap();
    let path = dir.path().join("graph.manifest");
    expected.save(&path).unwrap();
    next.replace(&path, &expected).unwrap();
    assert_eq!(
        GraphManifest::open(&path).unwrap().selected(),
        Some(second.as_path())
    );
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        expected.replace(&path, &expected).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(first).unwrap(), [37; 512]);
    assert_eq!(std::fs::read(second).unwrap(), [9; 512]);
    assert_eq!(dir.path().read_dir().unwrap().count(), 3);
}

#[test]
fn final_cancellation_and_destination_replacement_keep_staging_private() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image");
    std::fs::write(&image, [37; 512]).unwrap();
    let graph = ImageGraph::open(&[ImageSpec {
        path: image.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let expected = graph.manifest(None).unwrap();
    let next = graph.manifest(Some(&image)).unwrap();
    let path = dir.path().join("graph.manifest");
    expected.save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut observer = |event: OperationProgress| {
        assert_eq!(event.phase, OperationPhase::ManifestReplacement);
        ControlFlow::Break(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert_eq!(
        next.replace_with_context(&path, &expected, &mut context)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(dir.path().read_dir().unwrap().count(), 2);
    let mut observer = |_: OperationProgress| {
        std::fs::rename(&path, dir.path().join("original")).unwrap();
        std::fs::write(&path, b"foreign").unwrap();
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert!(
        next.replace_with_context(&path, &expected, &mut context)
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"foreign");
    assert_eq!(dir.path().read_dir().unwrap().count(), 3);
}

#[test]
fn locks_aliases_and_late_content_changes_refuse_without_overwrite() {
    use std::{fs::File, os::unix::fs::symlink};
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image");
    std::fs::write(&image, [37; 512]).unwrap();
    let graph = ImageGraph::open(&[ImageSpec {
        path: image.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let old = graph.manifest(None).unwrap();
    let next = graph.manifest(Some(&image)).unwrap();
    let path = dir.path().join("manifest");
    old.save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let lock = File::open(&path).unwrap();
    lock.try_lock().unwrap();
    assert!(next.replace(&path, &old).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(lock);
    let alias = dir.path().join("alias");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(next.replace(&path, &old).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_file(&alias).unwrap();
    symlink(&path, &alias).unwrap();
    assert!(next.replace(&alias, &old).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_file(alias).unwrap();
    let mut observer = |_: OperationProgress| {
        std::fs::write(&path, b"changed").unwrap();
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert!(
        next.replace_with_context(&path, &old, &mut context)
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"changed");
    assert_eq!(dir.path().read_dir().unwrap().count(), 2);
}

#[cfg(feature = "cli")]
#[test]
fn cli_selection_replaces_existing_manifest_after_exact_authorization() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image");
    std::fs::write(&image, [37; 512]).unwrap();
    let graph = ImageGraph::open(&[ImageSpec {
        path: image.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let old = graph.manifest(None).unwrap();
    let path = dir.path().join("manifest");
    old.save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let denied = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["graph", "select-in-place"])
        .arg(&path)
        .arg(&image)
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(2));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "graph", "select-in-place"])
        .arg(&path)
        .arg(&image)
        .arg(&image)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("manifest-replacement"));
    assert_eq!(
        GraphManifest::open(path).unwrap().selected(),
        Some(image.as_path())
    );
    assert_eq!(std::fs::read(image).unwrap(), [37; 512]);
}

#[test]
fn replacement_accepts_valid_native_encoding_of_unicode_paths() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image");
    std::fs::write(&image, [37; 512]).unwrap();
    let graph = ImageGraph::open(&[ImageSpec {
        path: image.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let old = graph.manifest(None).unwrap();
    let next = graph.manifest(Some(&image)).unwrap();
    let path = dir.path().join("manifest");
    old.save(&path).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes[17], 0);
    bytes[17] = 1;
    let payload = bytes.len() - 32;
    let digest = Sha256::digest(&bytes[..payload]);
    bytes[payload..].copy_from_slice(&digest);
    std::fs::write(&path, &bytes).unwrap();
    let expected = GraphManifest::open(&path).unwrap();
    assert_eq!(expected.images()[0].path, image);
    next.replace(&path, &expected).unwrap();
    assert_eq!(
        GraphManifest::open(&path).unwrap().selected(),
        Some(image.as_path())
    );
    assert_eq!(std::fs::read(image).unwrap(), [37; 512]);
}

#[cfg(feature = "cli")]
#[test]
fn cli_refuses_fifo_manifest_without_waiting_for_writer() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fifo");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_bits_truncate(0o600),
        0,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["graph", "info"])
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("manifest open waited for a FIFO writer");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("regular file"));
}

#[test]
fn equivalent_encoding_change_at_final_boundary_is_still_refused() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image");
    std::fs::write(&image, [37; 512]).unwrap();
    let graph = ImageGraph::open(&[ImageSpec {
        path: image.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let old = graph.manifest(None).unwrap();
    let next = graph.manifest(Some(&image)).unwrap();
    let path = dir.path().join("manifest");
    old.save(&path).unwrap();
    let mut changed = std::fs::read(&path).unwrap();
    changed[17] = 1;
    let payload = changed.len() - 32;
    let digest = Sha256::digest(&changed[..payload]);
    changed[payload..].copy_from_slice(&digest);
    let mut observer = |_: OperationProgress| {
        std::fs::write(&path, &changed).unwrap();
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert_eq!(
        next.replace_with_context(&path, &old, &mut context)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    assert_eq!(GraphManifest::open(path).unwrap().selected(), None);
    assert_eq!(dir.path().read_dir().unwrap().count(), 2);
}
