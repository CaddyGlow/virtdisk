use std::{cell::Cell, fs, io, ops::ControlFlow};
use virtdisk::{
    CheckOptions, ImageFormat, OperationContext, OperationLimits, OperationPhase, ParserLimits,
    check_image_with_limits, check_image_with_limits_and_context,
};

#[test]
fn invalid_parser_limits_precede_file_access_and_progress() {
    let directory = tempfile::tempdir().unwrap();
    let callbacks = Cell::new(0);
    let mut observer = |_| {
        callbacks.set(callbacks.get() + 1);
        ControlFlow::Continue(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    let error = check_image_with_limits_and_context(
        directory.path().join("absent"),
        ImageFormat::Raw,
        &[],
        CheckOptions::default(),
        ParserLimits {
            work_items: 0,
            ..Default::default()
        },
        &mut context,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(callbacks.get(), 0);
    assert_eq!(context.usage().io_operations, 0);
}

#[test]
fn parser_work_and_payload_context_have_independent_accounting() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    let original = [37; 65537];
    fs::write(&path, original).unwrap();
    let mut context = OperationContext::default();
    let error = check_image_with_limits_and_context(
        &path,
        ImageFormat::Raw,
        &[],
        CheckOptions { payload: true },
        ParserLimits {
            work_items: 1,
            ..Default::default()
        },
        &mut context,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(context.usage().logical_bytes, 65536);
    assert_eq!(context.usage().io_operations, 2);
    assert_eq!(fs::read(&path).unwrap(), original);

    let mut context = OperationContext::new(OperationLimits::default().logical_bytes(65536));
    let error = check_image_with_limits_and_context(
        &path,
        ImageFormat::Raw,
        &[],
        CheckOptions { payload: true },
        ParserLimits::default(),
        &mut context,
    )
    .unwrap_err();
    assert!(
        error
            .get_ref()
            .unwrap()
            .is::<virtdisk::OperationLimitExceeded>()
    );
    assert_eq!(context.usage().logical_bytes, 0);
    assert_eq!(context.usage().io_operations, 0);
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn caller_metadata_limits_cover_every_container_and_authorized_parents() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    fs::write(&raw, [37; 65536]).unwrap();
    let source = virtdisk::RawDisk::open(&raw).unwrap();
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let parent = directory.path().join(format!("parent-{format:?}"));
        let child = directory.path().join(format!("child-{format:?}"));
        virtdisk::convert_image(&source, &parent, format).unwrap();
        match format {
            ImageFormat::Qcow2 => {
                virtdisk::create_qcow2_overlay(&child, &parent, "qcow2", 65536).unwrap()
            }
            ImageFormat::Vhdx => virtdisk::create_vhdx_overlay(&child, &parent, &[]).unwrap(),
            ImageFormat::Vdi => virtdisk::create_vdi_overlay(&child, &parent, &[]).unwrap(),
            ImageFormat::Vmdk => drop(
                virtdisk::VmdkWriter::create_overlay(
                    &child,
                    &parent,
                    std::slice::from_ref(&parent),
                )
                .unwrap(),
            ),
            ImageFormat::Raw => unreachable!(),
        }
        let parent_bytes = fs::read(&parent).unwrap();
        let child_bytes = fs::read(&child).unwrap();
        let limits = ParserLimits {
            metadata_bytes: 1,
            ..Default::default()
        };
        assert!(
            check_image_with_limits(&parent, format, &[], CheckOptions::default(), limits).is_err()
        );
        assert!(
            check_image_with_limits(
                &child,
                format,
                std::slice::from_ref(&parent),
                CheckOptions::default(),
                limits
            )
            .is_err()
        );
        let report = check_image_with_limits(
            &child,
            format,
            std::slice::from_ref(&parent),
            CheckOptions { payload: true },
            ParserLimits::default(),
        )
        .unwrap();
        assert_eq!(report.payload_bytes_read, 65536);
        assert_eq!(fs::read(&parent).unwrap(), parent_bytes);
        assert_eq!(fs::read(&child).unwrap(), child_bytes);
    }
}

#[test]
fn context_cancellation_precedes_opening_with_valid_parser_limits() {
    let directory = tempfile::tempdir().unwrap();
    let mut observer = |progress: virtdisk::OperationProgress| {
        assert_eq!(progress.phase, OperationPhase::MetadataValidation);
        ControlFlow::Break(())
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    let error = check_image_with_limits_and_context(
        directory.path().join("absent"),
        ImageFormat::Raw,
        &[],
        CheckOptions::default(),
        ParserLimits::default(),
        &mut context,
    )
    .unwrap_err();
    assert!(
        error
            .get_ref()
            .unwrap()
            .is::<virtdisk::OperationCancelled>()
    );
    assert_eq!(context.usage().io_operations, 0);
}
