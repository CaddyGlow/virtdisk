use std::{io, ops::ControlFlow};
use virtdisk::{
    ImageFormat, ImageWriter, OperationContext, OperationLimits, OperationPhase, OperationProgress,
    ShrinkPolicy, WriteAt,
};

#[test]
fn zero_tail_accounting_and_final_cancellation_leave_container_unchanged() {
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        if !cfg!(target_os = "linux") && format != ImageFormat::Raw {
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image");
        let mut writer = ImageWriter::create_sparse(&path, format, 4096).unwrap();
        writer.write_all_at(0, &[37; 512]).unwrap();
        writer.flush().unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut observer = |event: OperationProgress| {
            if event.phase == OperationPhase::NativeResize {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let limits = OperationLimits::default()
            .logical_bytes(3584)
            .io_operations(8)
            .scratch_bytes(512)
            .unwrap();
        let mut context = OperationContext::new(limits).with_observer(&mut observer);
        assert_eq!(
            writer
                .resize_with_context(512, ShrinkPolicy::RequireZero, &mut context)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(context.usage().logical_bytes, 3584);
        assert_eq!(context.usage().io_operations, 7);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let mut context = OperationContext::new(limits);
        writer
            .resize_with_context(512, ShrinkPolicy::RequireZero, &mut context)
            .unwrap();
        assert_eq!(context.usage().io_operations, 8);
        assert_eq!(writer.len(), 512);
        let mut bytes = [0; 512];
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [37; 512]);
    }
}

#[test]
fn quota_refusal_and_nonzero_tail_precede_native_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let mut writer = ImageWriter::create(&path, ImageFormat::Raw, 4096).unwrap();
    writer.write_all_at(4095, &[37]).unwrap();
    let before = std::fs::read(&path).unwrap();
    for limits in [
        OperationLimits::default().logical_bytes(3583),
        OperationLimits::default().io_operations(1),
    ] {
        let mut context = OperationContext::new(limits);
        assert_eq!(
            writer
                .resize_with_context(512, ShrinkPolicy::RequireZero, &mut context)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(context.usage().io_operations, 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let mut context = OperationContext::default();
    assert_eq!(
        writer
            .resize_with_context(512, ShrinkPolicy::RequireZero, &mut context)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(context.usage().logical_bytes, 3584);
    assert_eq!(context.usage().io_operations, 1);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[cfg(all(feature = "cli", target_os = "linux"))]
#[test]
fn cli_controlled_backed_resize_requires_authorization_and_reports_boundary() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    let child = dir.path().join("child");
    std::fs::write(&parent, vec![37; 131072]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &parent, "raw", 65536).unwrap();
    let before = std::fs::read(&child).unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--operation-limit", "io=0", "resize-native"])
        .arg(&child)
        .args(["qcow2", "131072", "reject"])
        .arg(&parent)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    assert_eq!(std::fs::read(&child).unwrap(), before);
    let refused = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("resize-native")
        .arg(&child)
        .args(["qcow2", "131072", "reject"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    assert_eq!(std::fs::read(&child).unwrap(), before);
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "--operation-limit", "io=1", "resize-native"])
        .arg(&child)
        .args(["qcow2", "131072", "reject"])
        .arg(&parent)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("\"phase\":\"native-resize\""));
    let writer =
        ImageWriter::open_chain(&child, ImageFormat::Qcow2, std::slice::from_ref(&parent)).unwrap();
    let mut bytes = vec![0; 131072];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes[..65536].iter().all(|byte| *byte == 37));
    assert!(bytes[65536..].iter().all(|byte| *byte == 0));
    assert_eq!(std::fs::read(parent).unwrap(), vec![37; 131072]);
}

#[test]
fn reused_context_and_mid_scan_cancellation_preserve_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let mut writer = ImageWriter::create(&path, ImageFormat::Raw, 512).unwrap();
    let limits = OperationLimits::default()
        .logical_bytes(4096)
        .io_operations(2);
    let mut context = OperationContext::new(limits);
    writer
        .resize_with_context(4096, ShrinkPolicy::Reject, &mut context)
        .unwrap();
    assert_eq!(context.usage().logical_bytes, 0);
    assert_eq!(context.usage().io_operations, 1);
    writer
        .resize_with_context(512, ShrinkPolicy::RequireZero, &mut context)
        .unwrap_err();
    assert_eq!(writer.len(), 4096);
    assert_eq!(context.usage().io_operations, 1);
    let before = std::fs::read(&path).unwrap();
    let mut observer = |event: OperationProgress| {
        if event.phase == OperationPhase::TailValidation && event.completed_bytes == 512 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let limits = OperationLimits::default().scratch_bytes(512).unwrap();
    let mut context = OperationContext::new(limits).with_observer(&mut observer);
    assert_eq!(
        writer
            .resize_with_context(512, ShrinkPolicy::RequireZero, &mut context)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(context.usage().logical_bytes, 512);
    assert_eq!(context.usage().io_operations, 1);
    assert_eq!(std::fs::read(path).unwrap(), before);
}
