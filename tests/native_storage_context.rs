#![cfg(feature = "std")]
use std::ops::ControlFlow;
use virtdisk::io;
use virtdisk::{
    DiscardPolicy, ImageFormat, ImageWriter, OperationContext, OperationLimits, OperationPhase,
    OperationProgress, WriteAt,
};

#[test]
fn discard_refusal_and_cancellation_preserve_bytes_and_success_accounts_range() {
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
        let writer = ImageWriter::create_sparse(&path, format, 131072).unwrap();
        writer.write_all_at(0, &vec![37; 131072]).unwrap();
        writer.flush().unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut context = OperationContext::new(OperationLimits::default().logical_bytes(4096));
        assert_eq!(
            writer
                .discard_with_context(7, 4097, DiscardPolicy::AllowZeroFallback, &mut context)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(context.usage().io_operations, 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut observer = |event: OperationProgress| {
            assert_eq!(event.phase, OperationPhase::NativeDiscard);
            ControlFlow::Break(())
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        assert_eq!(
            writer
                .discard_with_context(7, 4097, DiscardPolicy::AllowZeroFallback, &mut context)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut context = OperationContext::new(
            OperationLimits::default()
                .logical_bytes(4097)
                .io_operations(1)
                .scratch_bytes(1)
                .unwrap(),
        );
        writer
            .discard_with_context(7, 4097, DiscardPolicy::AllowZeroFallback, &mut context)
            .unwrap();
        assert_eq!(context.usage().logical_bytes, 4097);
        assert_eq!(context.usage().io_operations, 1);
        assert_eq!(context.usage().peak_scratch_bytes, 0);
        let mut bytes = vec![0; 131072];
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert!(
            bytes[..7]
                .iter()
                .chain(&bytes[4104..])
                .all(|byte| *byte == 37)
        );
        assert!(bytes[7..4104].iter().all(|byte| *byte == 0));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn preallocation_keeps_content_and_counts_native_range_with_cumulative_quotas() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw");
    let writer = ImageWriter::create_sparse(&path, ImageFormat::Raw, 131072).unwrap();
    writer.write_all_at(7, b"retained").unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut observer = |event: OperationProgress| {
        assert_eq!(event.phase, OperationPhase::NativePreallocation);
        ControlFlow::Break(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert_eq!(
        writer
            .preallocate_with_context(0, 65536, &mut context)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(context.usage().io_operations, 0);
    let mut context = OperationContext::new(
        OperationLimits::default()
            .logical_bytes(65536)
            .io_operations(1),
    );
    writer
        .preallocate_with_context(0, 65536, &mut context)
        .unwrap();
    assert_eq!(context.usage().logical_bytes, 65536);
    assert_eq!(context.usage().io_operations, 1);
    assert!(
        writer
            .preallocate_with_context(65536, 65536, &mut context)
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn controlled_backed_discard_masks_parent_and_preserves_saved_inheritance() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    let child = dir.path().join("child");
    std::fs::write(&parent, vec![37; 65536]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &parent, "raw", 65536).unwrap();
    let mut writer =
        ImageWriter::open_chain(&child, ImageFormat::Qcow2, std::slice::from_ref(&parent)).unwrap();
    writer.create_snapshot(b"saved", b"saved").unwrap();
    let before = std::fs::read(&child).unwrap();
    let mut context = OperationContext::default();
    assert!(
        writer
            .discard_with_context(
                u64::MAX,
                512,
                DiscardPolicy::RequireDeallocation,
                &mut context
            )
            .is_err()
    );
    assert_eq!(context.usage().io_operations, 0);
    assert_eq!(std::fs::read(&child).unwrap(), before);
    writer
        .discard_with_context(0, 65536, DiscardPolicy::RequireDeallocation, &mut context)
        .unwrap();
    assert_eq!(context.usage().logical_bytes, 65536);
    assert_eq!(context.usage().io_operations, 1);
    writer.flush().unwrap();
    drop(writer);
    let disk = std::sync::Arc::new(
        virtdisk::Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap(),
    );
    disk.validate_active_mapping().unwrap();
    let mut bytes = vec![1; 65536];
    virtdisk::ReadAt::read_exact_at(&*disk, 0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|byte| *byte == 0));
    let saved = disk.open_snapshot(b"saved").unwrap();
    virtdisk::ReadAt::read_exact_at(&saved, 0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|byte| *byte == 37));
    assert_eq!(std::fs::read(parent).unwrap(), vec![37; 65536]);
}

#[cfg(all(feature = "cli", target_os = "linux"))]
#[test]
fn cli_storage_controls_refuse_quota_then_report_native_boundary() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw");
    std::fs::write(&path, vec![37; 131072]).unwrap();
    for command in ["preallocate", "trim"] {
        let before = std::fs::read(&path).unwrap();
        let mut process = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        process
            .args(["--operation-limit", "io=0", command])
            .arg(&path)
            .args(["raw", "0", "65536"]);
        if command == "trim" {
            process.arg("zero-fallback");
        }
        let refused = process.output().unwrap();
        assert_eq!(refused.status.code(), Some(2));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut process = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        process
            .args([
                "--progress",
                "--operation-limit",
                "bytes=65536",
                "--operation-limit",
                "io=1",
                command,
            ])
            .arg(&path)
            .args(["raw", "0", "65536"]);
        if command == "trim" {
            process.arg("zero-fallback");
        }
        let result = process.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let phase = if command == "trim" {
            "native-discard"
        } else {
            "native-preallocation"
        };
        assert!(String::from_utf8_lossy(&result.stderr).contains(phase));
        if command == "preallocate" {
            assert_eq!(std::fs::read(&path).unwrap(), before);
        } else {
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes[..65536].iter().all(|byte| *byte == 0));
            assert!(bytes[65536..].iter().all(|byte| *byte == 37));
        }
    }
}
