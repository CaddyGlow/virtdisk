#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use std::ops::ControlFlow;
use virtdisk::io;
use virtdisk::{
    ImageFormat, ImageWriter, OperationContext, OperationLimits, OperationPhase, OperationProgress,
    WriteAt,
};

#[test]
fn native_snapshot_controls_preserve_container_on_cancellation_and_quota_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let mut writer = ImageWriter::create_sparse(&path, ImageFormat::Qcow2, 65536).unwrap();
    writer.write_all_at(0, &[37; 512]).unwrap();
    writer.flush().unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut observer = |event: OperationProgress| {
        assert_eq!(event.phase, OperationPhase::NativeSnapshotCreation);
        assert_eq!(event.total_bytes, 0);
        ControlFlow::Break(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert_eq!(
        writer
            .create_snapshot_with_context(b"saved", b"saved", &mut context)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(context.usage().io_operations, 0);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let mut context = OperationContext::new(OperationLimits::default().io_operations(0));
    assert_eq!(
        writer
            .create_snapshot_with_context(b"saved", b"saved", &mut context)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let mut context =
        OperationContext::new(OperationLimits::default().logical_bytes(0).io_operations(3));
    writer
        .create_snapshot_with_context(b"saved", b"saved", &mut context)
        .unwrap();
    writer.write_all_at(0, &[9; 512]).unwrap();
    writer.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut cancel = |_: OperationProgress| ControlFlow::Break(());
    let mut canceled = OperationContext::default().with_observer(&mut cancel);
    assert_eq!(
        writer
            .revert_snapshot_with_context(b"saved", &mut canceled)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(
        writer
            .delete_snapshot_with_context(b"saved", &mut canceled)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    writer
        .revert_snapshot_with_context(b"saved", &mut context)
        .unwrap();
    let mut bytes = [0; 512];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 512]);
    writer
        .delete_snapshot_with_context(b"saved", &mut context)
        .unwrap();
    assert_eq!(context.usage().io_operations, 3);
    assert_eq!(context.usage().logical_bytes, 0);
    assert_eq!(context.usage().peak_scratch_bytes, 0);
    let before = std::fs::read(&path).unwrap();
    assert!(
        writer
            .create_snapshot_with_context(b"new", b"new", &mut context)
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn controlled_backed_lifecycle_preserves_parent_and_counts_failed_native_calls() {
    for format in [ImageFormat::Raw, ImageFormat::Qcow2] {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = dir.path().join("child");
        let base = ImageWriter::create_sparse(&parent, format, 65536).unwrap();
        base.write_all_at(0, &vec![37; 65536]).unwrap();
        base.flush().unwrap();
        drop(base);
        let parent_bytes = std::fs::read(&parent).unwrap();
        virtdisk::create_qcow2_overlay(
            &child,
            &parent,
            if format == ImageFormat::Raw {
                "raw"
            } else {
                "qcow2"
            },
            65536,
        )
        .unwrap();
        let mut writer =
            ImageWriter::open_chain(&child, ImageFormat::Qcow2, std::slice::from_ref(&parent))
                .unwrap();
        let mut events = Vec::new();
        let mut observer = |event: OperationProgress| {
            events.push(event);
            ControlFlow::Continue(())
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        writer
            .create_snapshot_with_context(b"saved", b"saved", &mut context)
            .unwrap();
        writer.write_all_at(0, &[9; 512]).unwrap();
        writer
            .revert_snapshot_with_context(b"saved", &mut context)
            .unwrap();
        let mut bytes = vec![0; 65536];
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert!(bytes.iter().all(|byte| *byte == 37));
        writer
            .delete_snapshot_with_context(b"saved", &mut context)
            .unwrap();
        let before = std::fs::read(&child).unwrap();
        assert!(
            writer
                .delete_snapshot_with_context(b"missing", &mut context)
                .is_err()
        );
        assert_eq!(context.usage().io_operations, 4);
        assert_eq!(std::fs::read(child).unwrap(), before);
        assert_eq!(std::fs::read(parent).unwrap(), parent_bytes);
        assert_eq!(
            events.iter().map(|event| event.phase).collect::<Vec<_>>(),
            [
                OperationPhase::NativeSnapshotCreation,
                OperationPhase::NativeSnapshotRevert,
                OperationPhase::NativeSnapshotDeletion,
                OperationPhase::NativeSnapshotDeletion
            ]
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.usage.io_operations)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }
}

#[cfg(feature = "cli")]
#[test]
fn cli_quotas_and_progress_cover_native_snapshot_lifecycle() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    drop(ImageWriter::create_sparse(&path, ImageFormat::Qcow2, 65536).unwrap());
    for action in ["create", "revert", "delete"] {
        let before = std::fs::read(&path).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        command
            .args(["--operation-limit", "io=0", "snapshot", action])
            .arg(&path)
            .arg("saved");
        if action == "create" {
            command.arg("saved");
        }
        let refused = command.output().unwrap();
        assert_eq!(refused.status.code(), Some(2));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut command = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        command
            .args([
                "--progress",
                "--operation-limit",
                "bytes=0",
                "--operation-limit",
                "io=1",
                "snapshot",
                action,
            ])
            .arg(&path)
            .arg("saved");
        if action == "create" {
            command.arg("saved");
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let phase = match action {
            "create" => "creation",
            "delete" => "deletion",
            _ => "revert",
        };
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(&format!("native-snapshot-{phase}"))
        );
    }
}
