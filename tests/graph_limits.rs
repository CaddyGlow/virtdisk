use virtdisk::{
    ImageFormat, ImageGraph, ImageSpec, ParserLimitExceeded, ParserLimits, ParserResource,
};

fn refusal(error: &std::io::Error) -> &ParserLimitExceeded {
    let mut current: &(dyn std::error::Error + 'static) = error;
    loop {
        if let Some(limit) = current.downcast_ref::<ParserLimitExceeded>() {
            return limit;
        }
        if let Some(io) = current.downcast_ref::<std::io::Error>()
            && let Some(inner) = io.get_ref()
        {
            current = inner;
            continue;
        }
        current = current.source().expect("missing typed parser refusal");
    }
}

#[test]
fn one_graph_budget_covers_registration_reopening_and_deferred_reads() {
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        drop(virtdisk::ImageWriter::create(&base, format, 512).unwrap());
        let specs = [ImageSpec {
            path: base.clone(),
            format,
            parent: None,
        }];
        let graph = ImageGraph::open_with_limits(&specs, ParserLimits::default()).unwrap();
        let budget = graph.budget().unwrap();
        let opened = budget.usage();
        assert!(opened.metadata_bytes > 0);
        assert!(opened.work_items > 0);
        let reader = graph.reader(&base).unwrap();
        let before = budget.usage();
        assert!(before.work_items > opened.work_items);
        let mut byte = [37];
        reader.read_exact_at(0, &mut byte).unwrap();
        assert_eq!(byte, [0]);
        assert_eq!(reader.budget().unwrap().usage(), budget.usage());
        assert!(budget.usage().work_items > before.work_items);
        let limits = ParserLimits {
            work_items: before.work_items,
            ..ParserLimits::default()
        };
        let graph = ImageGraph::open_with_limits(&specs, limits).unwrap();
        let reader = graph.reader(&base).unwrap();
        let failed = reader.read_exact_at(0, &mut byte).unwrap_err();
        assert_eq!(refusal(&failed).resource(), ParserResource::WorkItems);
        assert_eq!(
            graph.budget().unwrap().usage().work_items,
            before.work_items
        );
    }
}

#[test]
fn metadata_quotas_are_aggregate_across_independently_registered_images() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("image-a.raw");
    let b = dir.path().join("image-b.raw");
    std::fs::write(&a, [0; 512]).unwrap();
    std::fs::write(&b, [0; 512]).unwrap();
    let first = ImageSpec {
        path: a,
        format: ImageFormat::Raw,
        parent: None,
    };
    let second = ImageSpec {
        path: b,
        format: ImageFormat::Raw,
        parent: None,
    };
    let graph = ImageGraph::open_with_limits(std::slice::from_ref(&first), ParserLimits::default())
        .unwrap();
    let used = graph.budget().unwrap().usage().metadata_bytes;
    let limits = ParserLimits {
        metadata_bytes: used,
        ..ParserLimits::default()
    };
    ImageGraph::open_with_limits(std::slice::from_ref(&first), limits).unwrap();
    let failed = ImageGraph::open_with_limits(&[first, second], limits)
        .err()
        .unwrap();
    assert_eq!(refusal(&failed).resource(), ParserResource::MetadataBytes);
}

#[test]
fn invalid_limits_precede_path_access_and_chain_depth_uses_typed_budget_errors() {
    let missing = ImageSpec {
        path: "/nonexistent/virtdisk-graph".into(),
        format: ImageFormat::Raw,
        parent: None,
    };
    let failed = ImageGraph::open_with_limits(
        &[missing],
        ParserLimits {
            work_items: 0,
            ..ParserLimits::default()
        },
    )
    .err()
    .unwrap();
    assert_eq!(failed.kind(), std::io::ErrorKind::InvalidInput);
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.raw");
    let child = dir.path().join("child.qcow2");
    std::fs::write(&base, [37; 512]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", 512).unwrap();
    let specs = [
        ImageSpec {
            path: base.clone(),
            format: ImageFormat::Raw,
            parent: None,
        },
        ImageSpec {
            path: child,
            format: ImageFormat::Qcow2,
            parent: Some(base),
        },
    ];
    let failed = ImageGraph::open_with_limits(
        &specs,
        ParserLimits {
            recursion_depth: 1,
            ..ParserLimits::default()
        },
    )
    .err()
    .unwrap();
    assert_eq!(refusal(&failed).resource(), ParserResource::RecursionDepth);
}

#[test]
fn manifest_binding_can_share_limits_without_changing_legacy_graph_budgets() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.raw");
    std::fs::write(&base, [37; 512]).unwrap();
    let specs = [ImageSpec {
        path: base.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }];
    let graph = ImageGraph::open(&specs).unwrap();
    assert!(graph.budget().is_none());
    assert!(graph.reader(&base).unwrap().budget().is_none());
    let manifest = graph.manifest(Some(&base)).unwrap();
    let bounded = manifest
        .open_graph_with_limits(std::slice::from_ref(&base), ParserLimits::default())
        .unwrap();
    assert!(bounded.budget().is_some());
    let failed = manifest
        .open_graph_with_limits(
            std::slice::from_ref(&base),
            ParserLimits {
                metadata_bytes: 1,
                ..ParserLimits::default()
            },
        )
        .err()
        .unwrap();
    assert_eq!(refusal(&failed).resource(), ParserResource::MetadataBytes);
    let absent = dir.path().join("absent");
    let failed = manifest
        .open_graph_with_limits(
            &[absent],
            ParserLimits {
                work_items: 0,
                ..ParserLimits::default()
            },
        )
        .err()
        .unwrap();
    assert_eq!(failed.kind(), std::io::ErrorKind::InvalidInput);
    let legacy = manifest.open_graph(&[base]).unwrap();
    assert!(legacy.budget().is_none());
}

#[test]
fn staged_native_snapshot_parsing_shares_quota_and_failure_discards_output() {
    use std::{cell::Cell, ops::ControlFlow};
    use virtdisk::{OperationContext, OperationPhase, OperationProgress};
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let child = dir.path().join("child");
        drop(virtdisk::ImageWriter::create(&base, format, 512).unwrap());
        let specs = [ImageSpec {
            path: base.clone(),
            format,
            parent: None,
        }];
        let mut graph = ImageGraph::open_with_limits(&specs, ParserLimits::default()).unwrap();
        let budget = graph.budget().unwrap();
        let before_stage = Cell::new(0);
        let after_stage = Cell::new(0);
        let mut observer = |event: OperationProgress| {
            if event.phase == OperationPhase::MetadataValidation {
                before_stage.set(budget.usage().metadata_bytes);
            }
            if event.phase == OperationPhase::OutputVerification {
                after_stage.set(budget.usage().metadata_bytes);
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        assert_eq!(
            graph
                .snapshot_as_with_context(&base, &child, format, &mut context)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(after_stage.get() > before_stage.get());
        assert!(!child.exists());
        let mut graph = ImageGraph::open_with_limits(
            &specs,
            ParserLimits {
                metadata_bytes: before_stage.get(),
                ..ParserLimits::default()
            },
        )
        .unwrap();
        let error = graph.snapshot_as(&base, &child, format).unwrap_err();
        assert_eq!(refusal(&error).resource(), ParserResource::MetadataBytes);
        assert!(!child.exists());
        assert!(graph.children(&base).unwrap().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn authorized_inheritance_keeps_shared_accounting_after_graph_drop() {
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let child = dir.path().join("child");
        let writer = virtdisk::ImageWriter::create(&base, format, 512).unwrap();
        virtdisk::WriteAt::write_all_at(&writer, 0, &[37; 512]).unwrap();
        virtdisk::WriteAt::flush(&writer).unwrap();
        drop(writer);
        let mut graph = ImageGraph::open(&[ImageSpec {
            path: base.clone(),
            format,
            parent: None,
        }])
        .unwrap();
        graph.snapshot_as(&base, &child, format).unwrap();
        let manifest = graph.manifest(Some(&child)).unwrap();
        drop(graph);
        let graph = manifest
            .open_graph_with_limits(&[base, child.clone()], ParserLimits::default())
            .unwrap();
        let reader = graph.reader(&child).unwrap();
        let budget = graph.budget().unwrap();
        drop(graph);
        let before = budget.usage().work_items;
        let mut bytes = [0; 512];
        reader.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [37; 512]);
        assert!(budget.usage().work_items > before);
        assert_eq!(reader.budget().unwrap().usage(), budget.usage());
    }
}

#[test]
fn rebased_output_validation_uses_graph_quota_after_completed_difference_writes() {
    use std::{cell::Cell, ops::ControlFlow};
    use virtdisk::{OperationContext, OperationPhase, OperationProgress};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.raw");
    let parent = dir.path().join("parent.raw");
    let output = dir.path().join("rebased.qcow2");
    std::fs::write(&source, [37; 512]).unwrap();
    std::fs::write(&parent, [19; 512]).unwrap();
    let specs = [
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
    ];
    let mut graph = ImageGraph::open_with_limits(&specs, ParserLimits::default()).unwrap();
    let budget = graph.budget().unwrap();
    let before_stage = Cell::new(0);
    let after_stage = Cell::new(0);
    let mut observer = |event: OperationProgress| {
        if event.phase == OperationPhase::ImageExport && event.completed_bytes == 0 {
            before_stage.set(budget.usage().metadata_bytes);
        }
        if event.phase == OperationPhase::MetadataValidation {
            after_stage.set(budget.usage().metadata_bytes);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert_eq!(
        graph
            .rebase_to_with_context(&source, &parent, &output, &mut context)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(after_stage.get() > before_stage.get());
    let mut graph = ImageGraph::open_with_limits(
        &specs,
        ParserLimits {
            metadata_bytes: before_stage.get(),
            ..ParserLimits::default()
        },
    )
    .unwrap();
    let mut context = OperationContext::default();
    let error = graph
        .rebase_to_with_context(&source, &parent, &output, &mut context)
        .unwrap_err();
    assert_eq!(refusal(&error).resource(), ParserResource::MetadataBytes);
    assert_eq!(context.usage().logical_bytes, 512);
    assert_eq!(context.usage().io_operations, 3);
    assert!(!output.exists());
    assert!(graph.children(&parent).unwrap().is_empty());
    assert_eq!(std::fs::read(&source).unwrap(), [37; 512]);
    assert_eq!(std::fs::read(&parent).unwrap(), [19; 512]);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn a_short_symlink_cannot_hide_canonical_path_metadata_from_graph_limits() {
    let dir = tempfile::tempdir().unwrap();
    let mut parent = dir.path().to_path_buf();
    for _ in 0..10 {
        parent.push("long-directory-component-for-canonical-path-accounting");
    }
    std::fs::create_dir_all(&parent).unwrap();
    let target = parent.join("base.raw");
    std::fs::write(&target, [37; 512]).unwrap();
    assert!(target.as_os_str().as_encoded_bytes().len() > 512);
    let alias = dir.path().join("a");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let specs = [ImageSpec {
        path: alias,
        format: ImageFormat::Raw,
        parent: None,
    }];
    ImageGraph::open(&specs).unwrap();
    let error = ImageGraph::open_with_limits(
        &specs,
        ParserLimits {
            metadata_bytes: 512,
            ..ParserLimits::default()
        },
    )
    .err()
    .expect("canonical path must consume metadata quota");
    assert_eq!(refusal(&error).resource(), ParserResource::MetadataBytes);
}
