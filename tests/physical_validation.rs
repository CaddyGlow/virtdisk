use std::io;
use virtdisk::{
    PhysicalValidationBudget, PhysicalValidationLimitExceeded, PhysicalValidationLimits,
    PhysicalValidationResource, RawWriter,
};

#[test]
fn retained_handle_hashes_known_bytes_and_preserves_cumulative_usage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let writer = RawWriter::create(&path, 3).unwrap();
    writer.write_all_at(0, b"abc").unwrap();
    let limits = PhysicalValidationLimits::default()
        .physical_bytes(3)
        .unwrap()
        .read_calls(3)
        .unwrap()
        .scratch_bytes(1)
        .unwrap();
    let mut budget = PhysicalValidationBudget::new(limits);
    let fingerprint = writer.physical_fingerprint(&mut budget).unwrap();
    assert_eq!(fingerprint.len(), 3);
    assert_eq!(
        fingerprint.sha256(),
        &[
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad
        ]
    );
    let usage = budget.usage();
    assert_eq!(usage.physical_bytes, 3);
    assert_eq!(usage.hashed_bytes, 3);
    assert_eq!(usage.read_calls, 3);
    assert_eq!(usage.peak_scratch_bytes, 1);
    let error = writer.physical_fingerprint(&mut budget).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    let limit = error
        .get_ref()
        .unwrap()
        .downcast_ref::<PhysicalValidationLimitExceeded>()
        .unwrap();
    assert_eq!(limit.resource(), PhysicalValidationResource::PhysicalBytes);
    assert_eq!(limit.requested(), 6);
    assert_eq!(budget.usage(), usage);
    assert_eq!(std::fs::read(path).unwrap(), b"abc");
}

#[test]
fn read_quota_refusal_precedes_data_reads_and_empty_work_needs_no_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let writer = RawWriter::create(&path, 512).unwrap();
    let limits = PhysicalValidationLimits::default()
        .read_calls(1)
        .unwrap()
        .scratch_bytes(1)
        .unwrap();
    let mut budget = PhysicalValidationBudget::new(limits);
    let error = writer.physical_fingerprint(&mut budget).unwrap_err();
    assert_eq!(
        error
            .get_ref()
            .unwrap()
            .downcast_ref::<PhysicalValidationLimitExceeded>()
            .unwrap()
            .resource(),
        PhysicalValidationResource::ReadCalls
    );
    assert_eq!(budget.usage().read_calls, 0);
    assert_eq!(budget.usage().peak_scratch_bytes, 0);
    writer.resize(0).unwrap();
    let mut budget = PhysicalValidationBudget::new(
        PhysicalValidationLimits::default()
            .physical_bytes(0)
            .unwrap()
            .read_calls(0)
            .unwrap(),
    );
    let fingerprint = writer.physical_fingerprint(&mut budget).unwrap();
    assert!(fingerprint.is_empty());
    assert_eq!(budget.usage().physical_bytes, 0);
    assert_eq!(budget.usage().read_calls, 0);
    assert_eq!(budget.usage().peak_scratch_bytes, 0);
}

#[test]
fn invalid_configuration_and_changed_source_length_are_refused() {
    assert!(
        PhysicalValidationLimits::default()
            .physical_bytes(33 * 1024 * 1024 * 1024 + 1)
            .is_err()
    );
    assert!(
        PhysicalValidationLimits::default()
            .read_calls(1_048_577)
            .is_err()
    );
    assert!(
        PhysicalValidationLimits::default()
            .scratch_bytes(0)
            .is_err()
    );
    assert!(
        PhysicalValidationLimits::default()
            .scratch_bytes(65537)
            .is_err()
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    let writer = RawWriter::create(&path, 512).unwrap();
    // Bypass the advisory lock deliberately to exercise stale-length refusal.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(0)
        .unwrap();
    let mut budget = PhysicalValidationBudget::default();
    assert_eq!(
        writer.physical_fingerprint(&mut budget).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(budget.usage().read_calls, 0);
    assert_eq!(budget.usage().physical_bytes, 0);
    assert_eq!(budget.usage().peak_scratch_bytes, 0);
}
