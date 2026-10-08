use std::{error::Error, io};
use virtdisk::{ImageFormat, ImageOperation, ImageWriter, OperationError, WriteAt};

#[test]
fn common_writer_errors_preserve_operation_range_kind_and_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.raw");
    let writer = ImageWriter::create(&path, ImageFormat::Raw, 512).unwrap();
    let error = writer.write_all_at(511, &[1, 2]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    let context = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OperationError>()
        .unwrap();
    assert_eq!(context.operation(), ImageOperation::Write);
    assert_eq!(context.format(), ImageFormat::Raw);
    assert_eq!(context.range(), Some((511, 2)));
    assert!(
        context
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .is_some()
    );
    let mut bytes = [9; 512];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 512]);
    let error = writer.read_exact_at(512, &mut [0]).unwrap_err();
    let context = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OperationError>()
        .unwrap();
    assert_eq!(context.operation(), ImageOperation::Read);
    assert_eq!(context.range(), Some((512, 1)));
}

#[test]
fn unsupported_management_errors_are_structured_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.raw");
    let mut writer = ImageWriter::create(&path, ImageFormat::Raw, 512).unwrap();
    writer.write_all_at(0, &[37; 512]).unwrap();
    let error = writer.create_snapshot(b"1", b"saved").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    let context = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OperationError>()
        .unwrap();
    assert_eq!(context.operation(), ImageOperation::NativeSnapshotCreate);
    assert_eq!(context.range(), None);
    let error = writer
        .resize(256, virtdisk::ShrinkPolicy::Reject)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let context = error
        .get_ref()
        .unwrap()
        .downcast_ref::<OperationError>()
        .unwrap();
    assert_eq!(context.operation(), ImageOperation::Resize);
    assert_eq!(writer.len(), 512);
    let mut bytes = [0; 512];
    writer.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 512]);
}
