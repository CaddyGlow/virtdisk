use std::{error::Error, fs, io, sync::Arc};
use virtdisk::{ParserLimitExceeded, ParserLimits, ParserResource, RawDisk, ReadBudget};

fn quota(error: &io::Error) -> &ParserLimitExceeded {
    let mut current: &(dyn Error + 'static) = error;
    loop {
        if let Some(value) = current.downcast_ref::<ParserLimitExceeded>() {
            return value;
        }
        current = if let Some(value) = current.downcast_ref::<io::Error>() {
            value
                .get_ref()
                .map(|value| value as &(dyn Error + 'static))
                .or_else(|| current.source())
                .unwrap()
        } else {
            current.source().unwrap()
        };
    }
}

#[test]
fn failed_metadata_and_work_charges_preserve_usage_and_report_wide_requests() {
    let budget = ReadBudget::new(ParserLimits {
        metadata_bytes: 8,
        work_items: 1,
        ..Default::default()
    })
    .unwrap();
    budget.metadata(5).unwrap();
    let error = budget.metadata(4).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(quota(&error).resource(), ParserResource::MetadataBytes);
    assert_eq!(quota(&error).limit(), 8);
    assert_eq!(quota(&error).requested(), 9);
    assert_eq!(budget.usage().metadata_bytes, 5);
    budget.work(1).unwrap();
    let error = budget.work(u64::MAX).unwrap_err();
    assert_eq!(quota(&error).resource(), ParserResource::WorkItems);
    assert_eq!(quota(&error).requested(), u128::from(u64::MAX) + 1);
    assert_eq!(budget.usage().work_items, 1);
}

#[test]
fn cache_refusal_leaves_reservations_releasable() {
    let budget = ReadBudget::new(ParserLimits {
        cache_bytes: 4,
        ..Default::default()
    })
    .unwrap();
    let reservation = budget.cache(4).unwrap();
    let error = budget.cache(1).unwrap_err();
    assert_eq!(quota(&error).resource(), ParserResource::CacheBytes);
    assert_eq!(quota(&error).limit(), 4);
    assert_eq!(quota(&error).requested(), 5);
    assert_eq!(budget.usage().cache_bytes, 4);
    drop(reservation);
    assert_eq!(budget.usage().cache_bytes, 0);
    drop(budget.cache(4).unwrap());
    assert_eq!(budget.usage().cache_bytes, 0);
}

#[test]
fn decoder_unit_and_cumulative_limits_are_distinct_and_non_charging() {
    let budget = ReadBudget::new(ParserLimits {
        decompressed_bytes: 10,
        decompression_buffer_bytes: 8,
        ..Default::default()
    })
    .unwrap();
    budget.decode(3).unwrap();
    let error = budget.decode(8).unwrap_err();
    assert_eq!(quota(&error).resource(), ParserResource::DecompressedBytes);
    assert_eq!(quota(&error).limit(), 10);
    assert_eq!(quota(&error).requested(), 11);
    let error = budget.decode(9).unwrap_err();
    assert_eq!(
        quota(&error).resource(),
        ParserResource::DecompressionBufferBytes
    );
    assert_eq!(quota(&error).limit(), 8);
    assert_eq!(quota(&error).requested(), 9);
    assert_eq!(budget.usage().decompressed_bytes, 3);
}

#[test]
fn deferred_read_refusal_keeps_container_provenance_and_typed_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    fs::write(&path, [37; 2]).unwrap();
    let budget = ReadBudget::new(ParserLimits {
        work_items: 1,
        ..Default::default()
    })
    .unwrap();
    let reader = budget.reader(Arc::new(RawDisk::open(&path).unwrap()));
    reader.read_exact_at(0, &mut [0; 1]).unwrap();
    let error = reader.read_exact_at(1, &mut [0; 1]).unwrap_err();
    let provenance = error
        .get_ref()
        .unwrap()
        .downcast_ref::<virtdisk::ReadError>()
        .unwrap();
    assert_eq!(
        provenance.context.container.as_deref(),
        Some(path.as_path())
    );
    assert_eq!(quota(&error).resource(), ParserResource::WorkItems);
    assert_eq!(quota(&error).requested(), 2);
}

#[test]
fn concurrent_charges_report_the_observed_refused_total_without_overspending() {
    let budget = ReadBudget::new(ParserLimits {
        metadata_bytes: 8,
        ..Default::default()
    })
    .unwrap();
    let barrier = std::sync::Barrier::new(4);
    let results = std::thread::scope(|scope| {
        [(); 4]
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    budget.metadata(4)
                })
            })
            .map(|thread| thread.join().unwrap())
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 2);
    for error in results.into_iter().filter_map(Result::err) {
        assert_eq!(quota(&error).resource(), ParserResource::MetadataBytes);
        assert_eq!(quota(&error).limit(), 8);
        assert_eq!(quota(&error).requested(), 12);
    }
    assert_eq!(budget.usage().metadata_bytes, 8);
}
