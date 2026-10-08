#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use std::ops::ControlFlow;
use virtdisk::{
    ImageFormat, ImageGraph, ImageSpec, OperationContext, OperationLimits, OperationPhase,
};

fn graph(path: &std::path::Path, format: ImageFormat) -> ImageGraph {
    let writer = virtdisk::ImageWriter::create(path, format, 512).unwrap();
    virtdisk::WriteAt::write_all_at(&writer, 0, &[37; 512]).unwrap();
    virtdisk::WriteAt::flush(&writer).unwrap();
    drop(writer);
    ImageGraph::open(&[ImageSpec {
        path: path.into(),
        format,
        parent: None,
    }])
    .unwrap()
}

#[test]
fn cancellation_preserves_registered_leaf_and_source_bytes() {
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        for phase in [
            OperationPhase::MetadataValidation,
            OperationPhase::SnapshotDeletion,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().join("base");
            let child = dir.path().join("child");
            let mut graph = graph(&base, format);
            graph.snapshot_as(&base, &child, format).unwrap();
            let base_bytes = std::fs::read(&base).unwrap();
            let child_bytes = std::fs::read(&child).unwrap();
            let mut observer = |event: virtdisk::OperationProgress| {
                if event.phase == phase {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let mut context = OperationContext::default().with_observer(&mut observer);
            let error = graph
                .delete_snapshot_with_context(&child, &mut context)
                .unwrap_err();
            assert_eq!(error.kind(), virtdisk::io::ErrorKind::Interrupted);
            assert_eq!(
                graph.children(&base).unwrap().as_slice(),
                std::slice::from_ref(&child)
            );
            assert_eq!(std::fs::read(&base).unwrap(), base_bytes);
            assert_eq!(std::fs::read(&child).unwrap(), child_bytes);
        }
    }
}

#[test]
fn deletion_preserves_siblings_and_uses_no_payload_budget() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    let sibling = dir.path().join("sibling");
    let mut graph = graph(&base, ImageFormat::Qcow2);
    graph.snapshot(&base, &child).unwrap();
    graph.snapshot(&base, &sibling).unwrap();
    let mut phases = Vec::new();
    let mut observer = |event: virtdisk::OperationProgress| {
        phases.push(event.phase);
        ControlFlow::Continue(())
    };
    let limits = OperationLimits::default()
        .logical_bytes(0)
        .io_operations(0)
        .scratch_bytes(1)
        .unwrap();
    let mut context = OperationContext::new(limits).with_observer(&mut observer);
    graph
        .delete_snapshot_with_context(&child, &mut context)
        .unwrap();
    assert!(!child.exists());
    assert_eq!(context.usage(), virtdisk::OperationUsage::default());
    assert_eq!(
        phases,
        [
            OperationPhase::MetadataValidation,
            OperationPhase::SnapshotDeletion
        ]
    );
    assert_eq!(
        graph.children(&base).unwrap().as_slice(),
        std::slice::from_ref(&sibling)
    );
    let mut bytes = [0; 512];
    graph
        .reader(sibling)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [37; 512]);
}

#[test]
fn replacement_at_deletion_boundary_is_refused_without_unlinking() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    let retained = dir.path().join("retained");
    let mut graph = graph(&base, ImageFormat::Qcow2);
    graph.snapshot(&base, &child).unwrap();
    let original = std::fs::read(&child).unwrap();
    let mut observer = |event: virtdisk::OperationProgress| {
        if event.phase == OperationPhase::SnapshotDeletion {
            std::fs::rename(&child, &retained).unwrap();
            std::fs::copy(&retained, &child).unwrap();
        }
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert!(
        graph
            .delete_snapshot_with_context(&child, &mut context)
            .is_err()
    );
    assert_eq!(std::fs::read(child).unwrap(), original);
    assert_eq!(std::fs::read(retained).unwrap(), original);
}

#[test]
fn base_and_non_leaf_refusals_precede_final_deletion_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    let grandchild = dir.path().join("grandchild");
    let mut graph = graph(&base, ImageFormat::Qcow2);
    graph.snapshot(&base, &child).unwrap();
    graph.snapshot(&child, &grandchild).unwrap();
    for path in [&base, &child] {
        let mut phases = Vec::new();
        let mut observer = |event: virtdisk::OperationProgress| {
            phases.push(event.phase);
            ControlFlow::Continue(())
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let before = std::fs::read(path).unwrap();
        assert!(
            graph
                .delete_snapshot_with_context(path, &mut context)
                .is_err()
        );
        assert_eq!(phases, [OperationPhase::MetadataValidation]);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    assert_eq!(
        graph.children(&child).unwrap().as_slice(),
        std::slice::from_ref(&grandchild)
    );
}

#[test]
fn graph_parser_quota_refuses_deletion_without_unlinking() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let child = dir.path().join("child");
    std::fs::write(&base, [37; 512]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", 512).unwrap();
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
    let graph = ImageGraph::open_with_limits(&specs, virtdisk::ParserLimits::default()).unwrap();
    let work = graph.budget().unwrap().usage().work_items;
    let mut graph = ImageGraph::open_with_limits(
        &specs,
        virtdisk::ParserLimits {
            work_items: work,
            ..virtdisk::ParserLimits::default()
        },
    )
    .unwrap();
    let original = std::fs::read(&child).unwrap();
    let mut context = OperationContext::default();
    let error = graph
        .delete_snapshot_with_context(&child, &mut context)
        .unwrap_err();
    assert!(
        error
            .get_ref()
            .unwrap()
            .downcast_ref::<virtdisk::ParserLimitExceeded>()
            .is_some()
    );
    assert_eq!(std::fs::read(&child).unwrap(), original);
    assert_eq!(
        graph.children(&base).unwrap().as_slice(),
        std::slice::from_ref(&child)
    );
}
