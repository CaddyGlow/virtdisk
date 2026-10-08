#![cfg(feature = "std")]
use virtdisk::{ImageFormat, ImageGraph, OperationContext};
#[cfg(target_os = "linux")]
use virtdisk::{ImageSpec, OperationPhase, OperationProgress};

#[cfg(target_os = "linux")]
#[test]
fn generation_refuses_parent_replacement_at_commit_boundary() {
    use std::ops::ControlFlow;
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base");
    let retained = dir.path().join("retained-base");
    let output = dir.path().join("generation");
    let mut graph = base(&parent, ImageFormat::Qcow2);
    let mut observer = |event: OperationProgress| {
        if event.phase == OperationPhase::GenerationPublication {
            std::fs::rename(&parent, &retained).unwrap();
            std::fs::copy(&retained, &parent).unwrap();
        }
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert!(
        graph
            .snapshot_generation_with_context(&parent, &output, ImageFormat::Qcow2, &mut context)
            .is_err()
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    assert_eq!(
        std::fs::read(&parent).unwrap(),
        std::fs::read(&retained).unwrap()
    );
}

#[cfg(target_os = "linux")]
fn base(path: &std::path::Path, format: ImageFormat) -> ImageGraph {
    let writer = virtdisk::ImageWriter::create(path, format, 512).unwrap();
    virtdisk::WriteAt::write_all_at(&writer, 0, &[37; 512]).unwrap();
    virtdisk::WriteAt::flush(&writer).unwrap();
    drop(writer);
    ImageGraph::open_with_limits(
        &[ImageSpec {
            path: path.into(),
            format,
            parent: None,
        }],
        virtdisk::ParserLimits::default(),
    )
    .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn generation_publishes_image_and_manifest_together_and_registers_final_location() {
    use std::ops::ControlFlow;
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("base");
        let output = dir.path().join("generation");
        let mut graph = base(&parent, format);
        let original = std::fs::read(&parent).unwrap();
        let mut phases = Vec::new();
        let mut observer = |event: OperationProgress| {
            phases.push(event.phase);
            if event.phase == OperationPhase::GenerationPublication {
                assert!(!output.exists());
            }
            ControlFlow::Continue(())
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let manifest = graph
            .snapshot_generation_with_context(&parent, &output, format, &mut context)
            .unwrap();
        assert_eq!(context.usage().logical_bytes, 512);
        assert_eq!(context.usage().io_operations, 2);
        assert_eq!(phases.last(), Some(&OperationPhase::GenerationPublication));
        let image = output.join("image");
        assert_eq!(
            manifest.selected(),
            Some(image.canonicalize().unwrap().as_path())
        );
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 2);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        assert_eq!(
            graph.children(&parent).unwrap(),
            [image.canonicalize().unwrap()]
        );
        let loaded = virtdisk::GraphManifest::open(output.join("graph.manifest")).unwrap();
        let reopened = loaded.open_graph(&[parent.clone(), image.clone()]).unwrap();
        let mut bytes = [0; 512];
        reopened
            .reader(&image)
            .unwrap()
            .read_exact_at(0, &mut bytes)
            .unwrap();
        assert_eq!(bytes, [37; 512]);
        graph
            .reader(image)
            .unwrap()
            .read_exact_at(0, &mut bytes)
            .unwrap();
        assert_eq!(bytes, [37; 512]);
        assert_eq!(std::fs::read(parent).unwrap(), original);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn generation_cancellation_keeps_original_graph_and_removes_private_staging() {
    use std::ops::ControlFlow;
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        for phase in [
            OperationPhase::MetadataValidation,
            OperationPhase::OutputVerification,
            OperationPhase::Publication,
            OperationPhase::GenerationPublication,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("base");
            let output = dir.path().join("generation");
            let mut graph = base(&parent, format);
            let original = std::fs::read(&parent).unwrap();
            let mut observer = |event: OperationProgress| {
                if event.phase == phase
                    && (phase != OperationPhase::OutputVerification || event.completed_bytes > 0)
                {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let mut context = OperationContext::default().with_observer(&mut observer);
            assert_eq!(
                graph
                    .snapshot_generation_with_context(&parent, &output, format, &mut context)
                    .unwrap_err()
                    .kind(),
                virtdisk::io::ErrorKind::Interrupted
            );
            assert!(!output.exists());
            assert!(graph.children(&parent).unwrap().is_empty());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
            assert_eq!(std::fs::read(parent).unwrap(), original);
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn generation_no_overwrite_survives_a_final_boundary_destination_race() {
    use std::ops::ControlFlow;
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("base");
    let output = dir.path().join("generation");
    let mut graph = base(&parent, ImageFormat::Qcow2);
    let mut observer = |event: OperationProgress| {
        if event.phase == OperationPhase::GenerationPublication {
            std::fs::create_dir(&output).unwrap();
            std::fs::write(output.join("sentinel"), b"keep").unwrap();
        }
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert_eq!(
        graph
            .snapshot_generation_with_context(&parent, &output, ImageFormat::Qcow2, &mut context)
            .unwrap_err()
            .kind(),
        virtdisk::io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(output.join("sentinel")).unwrap(), b"keep");
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    assert!(graph.children(parent).unwrap().is_empty());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn generation_is_refused_before_io_on_platforms_without_the_publication_protocol() {
    let mut graph = ImageGraph::open(&[]).unwrap();
    let mut context = OperationContext::default();
    let error = graph
        .snapshot_generation_with_context(
            "missing",
            "missing/generation",
            ImageFormat::Qcow2,
            &mut context,
        )
        .unwrap_err();
    assert_eq!(error.kind(), virtdisk::io::ErrorKind::Unsupported);
    assert_eq!(context.usage(), virtdisk::OperationUsage::default());
    let error = graph
        .rebase_generation_with_context(
            "missing",
            "missing-parent",
            "missing/generation",
            &mut context,
        )
        .unwrap_err();
    assert_eq!(error.kind(), virtdisk::io::ErrorKind::Unsupported);
    assert_eq!(context.usage(), virtdisk::OperationUsage::default());
}

#[cfg(target_os = "linux")]
#[test]
fn rebase_generation_preserves_selected_content_and_publishes_final_topology() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    let source = dir.path().join("source");
    let output = dir.path().join("generation");
    std::fs::write(&parent, [37; 512]).unwrap();
    std::fs::write(&source, [91; 512]).unwrap();
    let mut graph = ImageGraph::open_with_limits(
        &[
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
        ],
        virtdisk::ParserLimits::default(),
    )
    .unwrap();
    let published = graph.rebase_generation(&source, &parent, &output).unwrap();
    let image = output.join("image");
    assert_eq!(published.selected(), Some(image.as_path()));
    assert_eq!(
        graph.children(&parent).unwrap().as_slice(),
        std::slice::from_ref(&image)
    );
    let manifest = virtdisk::GraphManifest::open(output.join("graph.manifest")).unwrap();
    let reopened = manifest
        .open_graph(&[parent.clone(), source.clone(), image.clone()])
        .unwrap();
    let mut bytes = [0; 512];
    reopened
        .reader(&image)
        .unwrap()
        .read_exact_at(0, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [91; 512]);
    assert_eq!(std::fs::read(parent).unwrap(), [37; 512]);
    assert_eq!(std::fs::read(source).unwrap(), [91; 512]);
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 2);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[cfg(target_os = "linux")]
#[test]
fn rebase_generation_cancellation_keeps_original_graph() {
    use std::ops::ControlFlow;
    for phase in [
        OperationPhase::ImageExport,
        OperationPhase::OutputVerification,
        OperationPhase::Publication,
        OperationPhase::GenerationPublication,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let output = dir.path().join("generation");
        let mut graph = base(&parent, ImageFormat::Qcow2);
        let original = std::fs::read(&parent).unwrap();
        let mut observer = |event: OperationProgress| {
            if event.phase == phase {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let error = graph
            .rebase_generation_with_context(&parent, &parent, &output, &mut context)
            .unwrap_err();
        assert_eq!(error.kind(), virtdisk::io::ErrorKind::Interrupted);
        assert!(!output.exists());
        assert!(graph.children(&parent).unwrap().is_empty());
        assert_eq!(std::fs::read(&parent).unwrap(), original);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
