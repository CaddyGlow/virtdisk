#![cfg(feature = "std")]
use virtdisk::{Capability, ImageOperation, InspectImage, RawDisk, RawWriter, UnsupportedReason};
#[test]
fn reports_enumerate_stable_operation_names_and_current_handle_access() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    let writer = RawWriter::create(&path, 512).unwrap();
    let mut before = vec![0; writer.len() as usize];
    writer.read_exact_at(0, &mut before).unwrap();
    let writer_capabilities = writer.inspection().capabilities;
    let rows: Vec<_> = writer_capabilities.iter().collect();
    assert_eq!(rows.len(), 16);
    let names: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|(operation, _)| operation.as_str())
        .collect();
    assert_eq!(names.len(), rows.len());
    for (operation, support) in rows {
        assert_eq!(writer_capabilities.get(operation), support);
    }
    assert!(names.contains("native-snapshot-create"));
    assert!(names.contains("extent-map"));
    assert_eq!(ImageOperation::WriteZeroes.as_str(), "write-zeroes");
    assert_eq!(
        UnsupportedReason::ReadOnlyHandle.as_str(),
        "read-only-handle"
    );
    assert_eq!(
        writer_capabilities.get(ImageOperation::Resize),
        Capability::Supported
    );
    drop(writer);
    let reader = RawDisk::open(&path).unwrap();
    let capabilities = reader.inspection().capabilities;
    assert!(
        capabilities
            .iter()
            .any(|(operation, support)| operation == ImageOperation::Read
                && support == Capability::Supported)
    );
    assert!(
        capabilities
            .iter()
            .any(|(operation, support)| operation == ImageOperation::Write
                && support == Capability::Unsupported(UnsupportedReason::ReadOnlyHandle))
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}
