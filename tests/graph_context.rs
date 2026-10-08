#![cfg(feature = "std")]
use std::{ops::ControlFlow, path::Path};
use virtdisk::{
    ImageFormat, ImageGraph, ImageSpec, OperationContext, OperationLimits, OperationPhase,
    OperationProgress,
};

fn graph(base: &Path, format: ImageFormat) -> ImageGraph {
    let writer = virtdisk::ImageWriter::create(base, format, 131584).unwrap();
    virtdisk::WriteAt::write_all_at(&writer, 0, &vec![37; 131584]).unwrap();
    virtdisk::WriteAt::flush(&writer).unwrap();
    drop(writer);
    ImageGraph::open(&[ImageSpec {
        path: base.into(),
        format,
        parent: None,
    }])
    .unwrap()
}

#[test]
fn native_snapshots_share_verification_budgets_and_register_only_after_publication() {
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        #[cfg(target_os = "linux")]
        ImageFormat::Vmdk,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let child = dir.path().join("child");
        let mut graph = graph(&base, format);
        let original = std::fs::read(&base).unwrap();
        let mut events = Vec::new();
        let mut observer = |event: OperationProgress| {
            events.push(event);
            ControlFlow::Continue(())
        };
        let mut context = OperationContext::new(
            OperationLimits::default()
                .logical_bytes(131584)
                .io_operations(6),
        )
        .with_observer(&mut observer);
        graph
            .snapshot_as_with_context(&base, &child, format, &mut context)
            .unwrap();
        assert_eq!(context.usage().logical_bytes, 131584);
        assert_eq!(context.usage().io_operations, 6);
        assert!(
            events
                .iter()
                .any(|e| e.phase == OperationPhase::OutputVerification
                    && e.completed_bytes == 131584)
        );
        assert_eq!(events.last().unwrap().phase, OperationPhase::Publication);
        assert_eq!(
            graph.children(&base).unwrap(),
            [child.canonicalize().unwrap()]
        );
        let mut bytes = vec![0; 131584];
        graph
            .reader(&child)
            .unwrap()
            .read_exact_at(0, &mut bytes)
            .unwrap();
        assert_eq!(bytes, vec![37; 131584]);
        assert_eq!(std::fs::read(base).unwrap(), original);
    }
}

#[test]
fn cancellation_at_verification_and_publication_discards_child_without_graph_edge() {
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        #[cfg(target_os = "linux")]
        ImageFormat::Vmdk,
    ] {
        for phase in [
            OperationPhase::OutputVerification,
            OperationPhase::Publication,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().join("base");
            let child = dir.path().join("child");
            let mut graph = graph(&base, format);
            let original = std::fs::read(&base).unwrap();
            let mut observer = |event: OperationProgress| {
                if event.phase == phase
                    && (phase == OperationPhase::Publication || event.completed_bytes > 0)
                {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let mut context = OperationContext::default().with_observer(&mut observer);
            let error = graph
                .snapshot_as_with_context(&base, &child, format, &mut context)
                .unwrap_err();
            assert_eq!(error.kind(), virtdisk::io::ErrorKind::Interrupted);
            assert!(!child.exists());
            assert!(graph.children(&base).unwrap().is_empty());
            assert_eq!(std::fs::read(base).unwrap(), original);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }
}

#[test]
fn flatten_and_merge_use_the_same_context_and_refuse_invalid_ancestry_before_callbacks() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    let mut graph = graph(&base, ImageFormat::Qcow2);
    graph.snapshot(&base, &child).unwrap();
    let mut context = OperationContext::default();
    let flat = dir.path().join("flat");
    graph
        .flatten_with_context(&child, &flat, ImageFormat::Raw, &mut context)
        .unwrap();
    let first = context.usage();
    assert_eq!(first.logical_bytes, 263168);
    let merged = dir.path().join("merged");
    graph
        .merge_to_with_context(&child, &base, &merged, ImageFormat::Raw, &mut context)
        .unwrap();
    assert_eq!(context.usage().logical_bytes, first.logical_bytes * 2);
    assert_eq!(std::fs::read(flat).unwrap(), std::fs::read(merged).unwrap());
    let mut observer = |_: OperationProgress| panic!("invalid ancestry must precede callbacks");
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert!(
        graph
            .merge_to_with_context(
                &base,
                &child,
                dir.path().join("invalid"),
                ImageFormat::Raw,
                &mut context
            )
            .is_err()
    );
    assert_eq!(context.usage(), virtdisk::OperationUsage::default());
}

#[test]
fn rebase_accounts_difference_copy_and_verification_and_preserves_original_branches() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.raw");
    let parent = dir.path().join("parent.raw");
    let output = dir.path().join("rebased.qcow2");
    let mut selected = vec![37; 131584];
    selected[65536..].fill(0);
    std::fs::write(&source, &selected).unwrap();
    std::fs::write(&parent, vec![37; 131584]).unwrap();
    let mut graph = ImageGraph::open(&[
        ImageSpec {
            path: source.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: parent.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
    ])
    .unwrap();
    let original_parent = std::fs::read(&parent).unwrap();
    let mut events = Vec::new();
    let mut observer = |event: OperationProgress| {
        events.push(event);
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::new(
        OperationLimits::default()
            .logical_bytes(263168)
            .io_operations(15),
    )
    .with_observer(&mut observer);
    graph
        .rebase_to_with_context(&source, &parent, &output, &mut context)
        .unwrap();
    assert_eq!(context.usage().logical_bytes, 263168);
    assert_eq!(context.usage().io_operations, 14);
    assert!(
        events
            .iter()
            .any(|e| e.phase == OperationPhase::ImageExport && e.completed_bytes == 131584)
    );
    assert_eq!(events.last().unwrap().phase, OperationPhase::Publication);
    let mut bytes = vec![0; 131584];
    graph
        .reader(&output)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, selected);
    assert_eq!(std::fs::read(source).unwrap(), selected);
    assert_eq!(std::fs::read(parent).unwrap(), original_parent);
}

#[test]
fn rebase_adapts_scratch_and_skips_reads_beyond_short_parent() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let parent = dir.path().join("parent");
    std::fs::write(&source, vec![37; 1024]).unwrap();
    std::fs::write(&parent, vec![37; 512]).unwrap();
    let mut graph = ImageGraph::open(&[
        ImageSpec {
            path: source.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: parent.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
    ])
    .unwrap();
    let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut observer = |_: OperationProgress| {
        notifications.set(notifications.get() + 1);
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::new(
        OperationLimits::default()
            .scratch_bytes(512)
            .unwrap()
            .logical_bytes(2048)
            .io_operations(16),
    )
    .with_observer(&mut observer);
    let output = dir.path().join("rebased");
    graph
        .rebase_to_with_context(&source, &parent, &output, &mut context)
        .unwrap();
    assert_eq!(context.usage().peak_scratch_bytes, 512);
    assert_eq!(context.usage().io_operations, 16);
    assert_eq!(context.usage().logical_bytes, 2048);
    assert!(notifications.get() > 0);
    let mut bytes = [0; 1024];
    graph
        .reader(&output)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [37; 1024]);
    let refused = dir.path().join("refused");
    assert!(
        graph
            .rebase_to_with_context(&source, &parent, &refused, &mut context)
            .unwrap_err()
            .get_ref()
            .unwrap()
            .is::<virtdisk::OperationLimitExceeded>()
    );
    assert!(!refused.exists());
    assert_eq!(graph.children(&parent).unwrap().len(), 1);
}

#[test]
fn rebase_cancellation_and_late_quota_refusal_remove_staging_and_preserve_graph() {
    for phase in [
        OperationPhase::ImageExport,
        OperationPhase::MetadataValidation,
        OperationPhase::OutputVerification,
        OperationPhase::Publication,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let parent = dir.path().join("parent");
        std::fs::write(&source, vec![0; 131584]).unwrap();
        std::fs::write(&parent, vec![37; 131584]).unwrap();
        let mut graph = ImageGraph::open(&[
            ImageSpec {
                path: source.clone(),
                format: ImageFormat::Raw,
                parent: None,
            },
            ImageSpec {
                path: parent.clone(),
                format: ImageFormat::Raw,
                parent: None,
            },
        ])
        .unwrap();
        let mut observer = |event: OperationProgress| {
            if event.phase == phase
                && (matches!(
                    phase,
                    OperationPhase::MetadataValidation | OperationPhase::Publication
                ) || event.completed_bytes > 0)
            {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let output = dir.path().join("cancelled");
        assert_eq!(
            graph
                .rebase_to_with_context(&source, &parent, &output, &mut context)
                .unwrap_err()
                .kind(),
            virtdisk::io::ErrorKind::Interrupted
        );
        assert!(!output.exists());
        assert!(graph.children(&parent).unwrap().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        assert_eq!(std::fs::read(&source).unwrap(), vec![0; 131584]);
        assert_eq!(std::fs::read(&parent).unwrap(), vec![37; 131584]);
        let mut context = OperationContext::new(OperationLimits::default().io_operations(14));
        assert!(
            graph
                .rebase_to_with_context(&source, &parent, &output, &mut context)
                .unwrap_err()
                .get_ref()
                .unwrap()
                .is::<virtdisk::OperationLimitExceeded>()
        );
        assert_eq!(context.usage().logical_bytes, 131584);
        assert_eq!(context.usage().io_operations, 9);
        assert!(!output.exists());
        assert!(graph.children(&parent).unwrap().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}

#[test]
fn snapshot_verification_quota_refusal_preserves_parent_and_graph() {
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        #[cfg(target_os = "linux")]
        ImageFormat::Vmdk,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let child = dir.path().join("child");
        let mut graph = graph(&base, format);
        let original = std::fs::read(&base).unwrap();
        let mut context = OperationContext::new(OperationLimits::default().logical_bytes(131583));
        assert!(
            graph
                .snapshot_as_with_context(&base, &child, format, &mut context)
                .unwrap_err()
                .get_ref()
                .unwrap()
                .is::<virtdisk::OperationLimitExceeded>()
        );
        assert_eq!(context.usage().logical_bytes, 0);
        assert_eq!(context.usage().io_operations, 0);
        assert!(!child.exists());
        assert!(graph.children(&base).unwrap().is_empty());
        assert_eq!(std::fs::read(base).unwrap(), original);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
#[ignore = "requires independent qemu-img graph materialization oracle"]
fn qemu_reads_controlled_snapshot_rebase_and_all_flattened_outputs() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.raw");
    let parent = dir.path().join("parent.raw");
    let child = dir.path().join("child.qcow2");
    let rebased = dir.path().join("rebased.qcow2");
    let expected: Vec<u8> = (0..131584)
        .map(|i| {
            if (65536..131072).contains(&i) {
                0
            } else {
                (i % 251) as u8
            }
        })
        .collect();
    std::fs::write(&base, &expected).unwrap();
    std::fs::write(&parent, vec![19; 65536]).unwrap();
    let mut graph = ImageGraph::open_with_limits(
        &[
            ImageSpec {
                path: base.clone(),
                format: ImageFormat::Raw,
                parent: None,
            },
            ImageSpec {
                path: parent.clone(),
                format: ImageFormat::Raw,
                parent: None,
            },
        ],
        virtdisk::ParserLimits::default(),
    )
    .unwrap();
    let mut context = OperationContext::default();
    graph
        .snapshot_with_context(&base, &child, &mut context)
        .unwrap();
    graph
        .rebase_to_with_context(&child, &parent, &rebased, &mut context)
        .unwrap();
    for path in [&child, &rebased] {
        let check = Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "{}",
            String::from_utf8_lossy(&check.stderr)
        );
        let raw = dir.path().join("oracle.raw");
        let converted = Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(path)
            .arg(&raw)
            .output()
            .unwrap();
        assert!(
            converted.status.success(),
            "{}",
            String::from_utf8_lossy(&converted.stderr)
        );
        assert_eq!(std::fs::read(&raw).unwrap(), expected);
        std::fs::remove_file(raw).unwrap();
    }
    for (format, name) in [
        (ImageFormat::Raw, "raw"),
        (ImageFormat::Qcow2, "qcow2"),
        (ImageFormat::Vhdx, "vhdx"),
        (ImageFormat::Vdi, "vdi"),
        (ImageFormat::Vmdk, "vmdk"),
    ] {
        let output = dir.path().join(format!("flattened.{name}"));
        graph
            .flatten_with_context(&rebased, &output, format, &mut context)
            .unwrap();
        let raw = dir.path().join("oracle.raw");
        let converted = Command::new("qemu-img")
            .args(["convert", "-f", name, "-O", "raw"])
            .arg(output)
            .arg(&raw)
            .output()
            .unwrap();
        assert!(
            converted.status.success(),
            "{}",
            String::from_utf8_lossy(&converted.stderr)
        );
        assert_eq!(std::fs::read(&raw).unwrap(), expected);
        std::fs::remove_file(raw).unwrap();
    }
    assert_eq!(std::fs::read(base).unwrap(), expected);
    assert_eq!(std::fs::read(parent).unwrap(), vec![19; 65536]);
}
