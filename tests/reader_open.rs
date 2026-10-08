#![cfg(feature = "std")]
use std::fs;
use virtdisk::io;

fn parser_quota(error: &io::Error) -> &virtdisk::ParserLimitExceeded {
    let mut current: &(dyn std::error::Error + 'static) = error;
    loop {
        if let Some(value) = current.downcast_ref::<virtdisk::ParserLimitExceeded>() {
            return value;
        }
        current = if let Some(value) = current.downcast_ref::<io::Error>() {
            value
                .get_ref()
                .map(|value| value as &(dyn std::error::Error + 'static))
                .or_else(|| current.source())
                .expect("missing typed parser limit")
        } else {
            current.source().expect("missing typed parser limit")
        };
    }
}

#[test]
fn vmdk_descriptor_and_grain_table_limits_report_requested_attribute_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    let image = directory.path().join("image.vmdk");
    fs::write(&raw, [37; 65536]).unwrap();
    virtdisk::convert_image(
        &virtdisk::RawDisk::open(&raw).unwrap(),
        &image,
        ImageFormat::Vmdk,
    )
    .unwrap();
    let mut original = fs::read(&image).unwrap();
    let descriptor_bytes = u64::from_le_bytes(original[36..44].try_into().unwrap()) * 512;
    let options = ReaderOpenOptions::default()
        .format(ImageFormat::Vmdk)
        .parser_limits(ParserLimits {
            attribute_bytes: descriptor_bytes - 1,
            ..Default::default()
        })
        .unwrap();
    let error = Image::open_with_options(&image, &options).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::ResourceLimit);
    assert_eq!(
        parser_quota(&error).resource(),
        virtdisk::ParserResource::AttributeBytes
    );
    assert_eq!(
        parser_quota(&error).requested(),
        u128::from(descriptor_bytes)
    );
    assert_eq!(fs::read(&image).unwrap(), original);
    // A hosted sparse image can omit its embedded descriptor. Its tables
    // remain independently bounded, even when no descriptor is materialized.
    original[28..44].fill(0);
    fs::write(&image, &original).unwrap();
    drop(
        Image::open_with_options(
            &image,
            &ReaderOpenOptions::default().format(ImageFormat::Vmdk),
        )
        .unwrap(),
    );
    let table_bytes = u64::from(u32::from_le_bytes(original[44..48].try_into().unwrap())) * 4;
    let options = ReaderOpenOptions::default()
        .format(ImageFormat::Vmdk)
        .parser_limits(ParserLimits {
            attribute_bytes: table_bytes - 1,
            ..Default::default()
        })
        .unwrap();
    let error = Image::open_with_options(&image, &options).err().unwrap();
    assert_eq!(
        parser_quota(&error).resource(),
        virtdisk::ParserResource::AttributeBytes
    );
    assert_eq!(parser_quota(&error).limit(), table_bytes - 1);
    assert_eq!(parser_quota(&error).requested(), u128::from(table_bytes));
    assert_eq!(fs::read(&image).unwrap(), original);
}

#[test]
fn authorized_chains_report_typed_recursion_limits() {
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
        #[cfg(not(target_os = "linux"))]
        if format == ImageFormat::Vmdk {
            let limits = ParserLimits {
                recursion_depth: 1,
                ..Default::default()
            };
            let options = ReaderOpenOptions::default()
                .format(format)
                .parser_limits(limits)
                .unwrap();
            let image = Image::open_with_options(&parent, &options).unwrap();
            assert_eq!(image.budget().unwrap().limits(), limits);
            let mut observed = vec![0; 65536];
            image.read_exact_at(0, &mut observed).unwrap();
            assert_eq!(observed, [37; 65536]);
            drop(image);
            assert_vmdk_overlay_refusal(&child, &parent);
            continue;
        }
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
        let original = fs::read(&child).unwrap();
        let options = ReaderOpenOptions::default()
            .format(format)
            .authorized_paths([parent])
            .parser_limits(ParserLimits {
                recursion_depth: 1,
                ..Default::default()
            })
            .unwrap();
        let error = Image::open_with_options(&child, &options).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::ResourceLimit);
        let mut current: &(dyn std::error::Error + 'static) = &error;
        let found = loop {
            if let Some(value) = current.downcast_ref::<virtdisk::ParserLimitExceeded>() {
                break value;
            }
            current = if let Some(value) = current.downcast_ref::<io::Error>() {
                value
                    .get_ref()
                    .map(|value| value as &(dyn std::error::Error + 'static))
                    .or_else(|| current.source())
                    .unwrap()
            } else {
                current.source().unwrap()
            };
        };
        assert_eq!(found.resource(), virtdisk::ParserResource::RecursionDepth);
        assert_eq!(found.limit(), 1);
        assert_eq!(found.requested(), 2);
        assert_eq!(fs::read(&child).unwrap(), original);
    }
}
use virtdisk::{
    Image, ImageFormat, InspectImage, ParserLimits, ReadAt, ReadRecoveryPolicy, ReaderOpenOptions,
};

#[test]
fn recognized_containers_and_authorized_chains_retain_limits_and_source_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let capacity = 65536;
    let model = vec![37; capacity];
    let raw = directory.path().join("raw");
    fs::write(&raw, &model).unwrap();
    let source = virtdisk::RawDisk::open(&raw).unwrap();
    let limits = ParserLimits {
        work_items: 100000,
        ..Default::default()
    };
    for format in [
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let parent = directory.path().join(format!("parent-{format:?}"));
        let child = directory.path().join(format!("child-{format:?}"));
        virtdisk::convert_image(&source, &parent, format).unwrap();
        let options = ReaderOpenOptions::default().parser_limits(limits).unwrap();
        let image = Image::open_with_options(&parent, &options).unwrap();
        assert_eq!(image.info().format, format);
        assert_eq!(image.budget().unwrap().limits(), limits);
        let mut observed = vec![0; capacity];
        image.read_exact_at(0, &mut observed).unwrap();
        assert_eq!(observed, model);
        drop(image);
        #[cfg(not(target_os = "linux"))]
        if format == ImageFormat::Vmdk {
            assert_vmdk_overlay_refusal(&child, &parent);
            continue;
        }
        match format {
            ImageFormat::Qcow2 => {
                virtdisk::create_qcow2_overlay(&child, &parent, "qcow2", capacity as u64).unwrap()
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
        assert!(Image::open_with_options(&child, &options).is_err());
        assert!(Image::open_with_options(&child, &options.clone().authorized_paths([])).is_err());
        let authorized = options.clone().authorized_paths([parent.clone()]);
        let image = Image::open_with_options(&child, &authorized).unwrap();
        assert!(image.inspection().has_parent);
        assert_eq!(image.budget().unwrap().limits(), limits);
        image.read_exact_at(0, &mut observed).unwrap();
        assert_eq!(observed, model, "{format:?} inheritance");
        assert_eq!(fs::read(&parent).unwrap(), parent_bytes);
        assert_eq!(fs::read(&child).unwrap(), child_bytes);
        let tight = authorized
            .parser_limits(ParserLimits {
                metadata_bytes: 1,
                ..Default::default()
            })
            .unwrap();
        assert!(Image::open_with_options(&child, &tight).is_err());
        assert_eq!(fs::read(&parent).unwrap(), parent_bytes);
        assert_eq!(fs::read(&child).unwrap(), child_bytes);
    }
}

#[test]
fn descriptor_extents_require_explicit_format_and_complete_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let descriptor = directory.path().join("disk.vmdk");
    let first = directory.path().join("first extent.bin");
    let second = directory.path().join("second.bin");
    fs::write(&first, [3; 1024]).unwrap();
    fs::write(&second, [8; 512]).unwrap();
    let text = "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\nRW 1 FLAT \"first extent.bin\" 1\nRW 1 FLAT \"second.bin\" 0\n";
    fs::write(&descriptor, text).unwrap();
    let options = ReaderOpenOptions::default();
    assert!(
        Image::open_with_options(
            &descriptor,
            &options
                .clone()
                .authorized_paths([first.clone(), second.clone()])
        )
        .is_err()
    );
    let options = options.format(ImageFormat::Vmdk);
    assert!(Image::open_with_options(&descriptor, &options).is_err());
    assert!(
        Image::open_with_options(
            &descriptor,
            &options.clone().authorized_paths([first.clone()])
        )
        .is_err()
    );
    let limits = ParserLimits {
        work_items: 1000,
        ..Default::default()
    };
    let options = options
        .authorized_paths([first.clone(), second.clone()])
        .parser_limits(limits)
        .unwrap();
    let image = Image::open_with_options(&descriptor, &options).unwrap();
    let mut observed = [0; 8];
    image.read_exact_at(508, &mut observed).unwrap();
    assert_eq!(observed, [3, 3, 3, 3, 8, 8, 8, 8]);
    assert_eq!(image.budget().unwrap().limits(), limits);
    assert_eq!(
        image.inspection().container_set_size,
        Some(text.len() as u64 + 1536)
    );
    assert_eq!(fs::read(&descriptor).unwrap(), text.as_bytes());
    assert_eq!(fs::read(&first).unwrap(), [3; 1024]);
    assert_eq!(fs::read(&second).unwrap(), [8; 512]);
}

#[test]
fn raw_deferred_reads_share_the_caller_work_budget() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    fs::write(&path, [37; 512]).unwrap();
    let options = ReaderOpenOptions::default()
        .format(ImageFormat::Raw)
        .parser_limits(ParserLimits {
            work_items: 1,
            ..Default::default()
        })
        .unwrap();
    let image = Image::open_with_options(&path, &options).unwrap();
    image.read_exact_at(0, &mut [0; 1]).unwrap();
    assert_eq!(
        image.read_exact_at(1, &mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::ResourceLimit
    );
    assert_eq!(image.budget().unwrap().usage().work_items, 1);
}

#[test]
fn every_container_enforces_tight_metadata_limits() {
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
        let path = directory.path().join(format!("{format:?}"));
        virtdisk::convert_image(&source, &path, format).unwrap();
        let original = fs::read(&path).unwrap();
        let options = ReaderOpenOptions::default()
            .format(format)
            .parser_limits(ParserLimits {
                metadata_bytes: 1,
                ..Default::default()
            })
            .unwrap();
        assert!(
            Image::open_with_options(&path, &options).is_err(),
            "{format:?} ignored limits"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[test]
fn incompatible_recovery_and_invalid_limits_are_explicit_errors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    fs::write(&path, [37; 512]).unwrap();
    let options = ReaderOpenOptions::default()
        .format(ImageFormat::Raw)
        .recovery_policy(ReadRecoveryPolicy::ReplayVhdxLog);
    assert_eq!(
        Image::open_with_options(&path, &options)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert!(
        ReaderOpenOptions::default()
            .parser_limits(ParserLimits {
                work_items: 0,
                ..Default::default()
            })
            .is_err()
    );
}

#[test]
fn recognition_never_falls_back_and_raw_requires_explicit_selection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    fs::write(&path, b"unknown format").unwrap();
    assert!(Image::open_with_options(&path, &ReaderOpenOptions::default()).is_err());
    fs::write(&path, b"QFI\xfb").unwrap();
    assert!(Image::open_with_options(&path, &ReaderOpenOptions::default()).is_err());
    let image = Image::open_with_options(
        &path,
        &ReaderOpenOptions::default().format(ImageFormat::Raw),
    )
    .unwrap();
    assert_eq!(image.len(), 4);
}

#[cfg(not(target_os = "linux"))]
fn assert_vmdk_overlay_refusal(child: &std::path::Path, parent: &std::path::Path) {
    let original = fs::read(parent).unwrap();
    let error = virtdisk::VmdkWriter::create_overlay(
        child,
        parent,
        std::slice::from_ref(&parent.to_path_buf()),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(!child.exists());
    assert_eq!(fs::read(parent).unwrap(), original);
}
