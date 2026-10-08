#![cfg(feature = "std")]
use std::ops::ControlFlow;
use virtdisk::{ImageFormat, ImageGraph, ImageSpec, OperationContext, OperationPhase};

#[test]
fn selection_cancellation_never_publishes_or_changes_original_manifest() {
    for phase in [
        OperationPhase::MetadataValidation,
        OperationPhase::Publication,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("image");
        let original = dir.path().join("original.manifest");
        let output = dir.path().join("selected.manifest");
        std::fs::write(&image, [37; 512]).unwrap();
        let graph = ImageGraph::open(&[ImageSpec {
            path: image.clone(),
            format: ImageFormat::Raw,
            parent: None,
        }])
        .unwrap();
        graph.manifest(None).unwrap().save(&original).unwrap();
        let before = std::fs::read(&original).unwrap();
        let mut observer = |event: virtdisk::OperationProgress| {
            if event.phase == phase {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut context = OperationContext::default().with_observer(&mut observer);
        let error = graph
            .save_manifest_with_context(Some(&image), &output, &mut context)
            .unwrap_err();
        assert_eq!(error.kind(), virtdisk::io::ErrorKind::Interrupted);
        assert!(!output.exists());
        assert_eq!(std::fs::read(&original).unwrap(), before);
        assert_eq!(std::fs::read(&image).unwrap(), [37; 512]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}

#[test]
fn saving_selection_revalidates_registration_and_can_clear_selection() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image");
    let unregistered = dir.path().join("unregistered");
    std::fs::write(&image, [37; 512]).unwrap();
    std::fs::write(&unregistered, [91; 512]).unwrap();
    let graph = ImageGraph::open(&[ImageSpec {
        path: image.clone(),
        format: ImageFormat::Raw,
        parent: None,
    }])
    .unwrap();
    let output = dir.path().join("refused.manifest");
    assert!(graph.save_manifest(Some(&unregistered), &output).is_err());
    assert!(!output.exists());
    let selected = dir.path().join("selected.manifest");
    let result = graph.save_manifest(Some(&image), &selected).unwrap();
    let canonical = image.canonicalize().unwrap();
    assert_eq!(result.selected(), Some(canonical.as_path()));
    let clear = dir.path().join("clear.manifest");
    let result = graph.save_manifest(None, &clear).unwrap();
    assert!(result.selected().is_none());
    assert!(
        virtdisk::GraphManifest::open(clear)
            .unwrap()
            .selected()
            .is_none()
    );
}
