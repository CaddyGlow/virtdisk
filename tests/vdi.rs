use std::{io, sync::Arc};
use virtdisk::{ParserLimits, ReadAt, Vdi};
#[path = "support/bytes.rs"]
mod bytes;
use bytes::Bytes;

fn put(b: &mut [u8], at: usize, value: u32) {
    b[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn fixture(kind: u32, entries: &[u32], allocated: u32) -> Vec<u8> {
    let mut b = vec![0; 1024 + allocated as usize * 512];
    put(&mut b, 64, 0xbeda107f);
    put(&mut b, 68, 0x10001);
    put(&mut b, 72, 400);
    put(&mut b, 76, kind);
    put(&mut b, 340, 512);
    put(&mut b, 344, 1024);
    put(&mut b, 360, 512);
    b[368..376].copy_from_slice(&(entries.len() as u64 * 512).to_le_bytes());
    put(&mut b, 376, 512);
    put(&mut b, 384, entries.len() as u32);
    put(&mut b, 388, allocated);
    b[392..408].fill(7);
    b[408..424].fill(8);
    for (i, &e) in entries.iter().enumerate() {
        put(&mut b, 512 + 4 * i, e);
    }
    for i in 0..allocated as usize {
        b[1024 + i * 512..1024 + (i + 1) * 512].fill(i as u8 + 17);
    }
    b
}
fn open(b: Vec<u8>) -> io::Result<Vdi> {
    Vdi::open(Arc::new(Bytes(b)))
}
#[test]
fn dynamic_permuted_blocks_free_and_zero_are_read_logically() {
    let disk = open(fixture(1, &[1, u32::MAX, u32::MAX - 1, 0], 2)).unwrap();
    let mut b = vec![9; 2048];
    disk.read_exact_at(0, &mut b).unwrap();
    assert!(b[..512].iter().all(|&v| v == 18));
    assert!(b[512..1536].iter().all(|&v| v == 0));
    assert!(b[1536..].iter().all(|&v| v == 17));
    let mut cross = [0; 4];
    disk.read_exact_at(510, &mut cross).unwrap();
    assert_eq!(cross, [18, 18, 0, 0]);
    assert!(disk.read_exact_at(2048, &mut []).is_ok());
    assert!(disk.read_exact_at(2049, &mut []).is_err());
    assert!(disk.read_exact_at(u64::MAX, &mut cross).is_err());
}
#[test]
fn fixed_images_require_fully_owned_unique_allocations() {
    assert!(open(fixture(2, &[1, 0], 2)).is_ok());
    for entries in [[0, 0], [0, u32::MAX], [0, 2]] {
        assert!(open(fixture(2, &entries, 2)).is_err());
    }
    assert!(open(fixture(1, &[0, 0], 2)).is_err());
    assert!(open(fixture(1, &[u32::MAX], 1)).is_err());
}
#[test]
fn invalid_headers_overlapping_metadata_truncation_and_parents_rejected() {
    for (at, value) in [
        (64, 0),
        (68, 0x10002),
        (72, 399),
        (76, 4),
        (80, 0x8000),
        (340, 400),
        (344, 512),
        (360, 4096),
        (376, 511),
        (384, 3),
        (388, 3),
    ] {
        let mut b = fixture(1, &[0, 1], 2);
        put(&mut b, at, value);
        assert!(open(b).is_err(), "field {at}");
    }
    let mut b = fixture(1, &[0], 1);
    b[424] = 1;
    assert!(open(b).is_err());
    let mut b = fixture(1, &[0], 1);
    b.pop();
    assert!(open(b).is_err());
}
#[test]
fn parser_limits_bound_metadata_cache_and_deferred_work() {
    for limits in [
        ParserLimits {
            metadata_bytes: 100,
            ..Default::default()
        },
        ParserLimits {
            cache_bytes: 1,
            ..Default::default()
        },
        ParserLimits {
            work_items: 1,
            ..Default::default()
        },
    ] {
        assert!(Vdi::open_with_limits(Arc::new(Bytes(fixture(1, &[0], 1))), limits).is_err());
    }
    let disk = Vdi::open_with_limits(
        Arc::new(Bytes(fixture(1, &[0], 1))),
        ParserLimits {
            work_items: 5,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(disk.read_exact_at(0, &mut [0]).is_ok());
    assert!(disk.read_exact_at(0, &mut [0]).is_err());
}

#[test]
fn block_service_prefix_is_not_exposed_and_short_final_block_is_bounded() {
    let mut b = fixture(1, &[0], 1);
    put(&mut b, 380, 512);
    b.resize(2048, 0xee);
    b[1024..1536].fill(0xab);
    let disk = open(b).unwrap();
    let mut out = [0; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0xee; 512]);
    let mut b = fixture(1, &[0], 1);
    put(&mut b, 376, 1024);
    b.resize(2048, 0xcc);
    let disk = open(b).unwrap();
    assert_eq!(disk.len(), 512);
    assert!(disk.read_exact_at(511, &mut [0; 2]).is_err());
}

#[test]
fn extent_visitor_distinguishes_allocated_free_and_zero() {
    use virtdisk::ExtentKind;
    let d = open(fixture(1, &[0, u32::MAX, u32::MAX - 1], 1)).unwrap();
    let mut extents = Vec::new();
    d.visit_extents(&mut |e| {
        extents.push(e);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        extents
            .iter()
            .map(|e| (e.offset, e.length, e.kind))
            .collect::<Vec<_>>(),
        [
            (0, 512, ExtentKind::Allocated),
            (512, 512, ExtentKind::Zero),
            (1024, 512, ExtentKind::Zero)
        ]
    );
    let mut count = 0;
    assert!(
        d.visit_extents(&mut |_| {
            count += 1;
            Err(io::Error::other("cancel"))
        })
        .is_err()
    );
    assert_eq!(count, 1);
}

#[test]
fn extent_visitation_clips_last_block_and_charges_each_callback() {
    let mut b = fixture(1, &[0, u32::MAX], 1);
    put(&mut b, 376, 1024);
    b.resize(2048, 0);
    b[368..376].copy_from_slice(&1536u64.to_le_bytes());
    let disk = Vdi::open_with_limits(
        Arc::new(Bytes(b)),
        ParserLimits {
            work_items: 32,
            ..Default::default()
        },
    )
    .unwrap();
    let mut extents = Vec::new();
    disk.visit_extents(&mut |e| {
        extents.push(e);
        Ok(())
    })
    .unwrap();
    assert_eq!(extents[1].length, 512);
    let mut exhausted = false;
    for _ in 0..32 {
        if disk.visit_extents(&mut |_| Ok(())).is_err() {
            exhausted = true;
            break;
        }
    }
    assert!(exhausted);
}

#[test]
fn native_header_rejects_nil_creation_and_modification_ids() {
    for kind in [1, 2] {
        for header_size in [400, 416] {
            for at in [392, 408] {
                let mut bytes = fixture(kind, &[0], 1);
                put(&mut bytes, 72, header_size);
                bytes[at..at + 16].fill(0);
                let error = open(bytes)
                    .err()
                    .expect("nil native image identity must be rejected");
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            }
        }
    }
}
#[test]
fn native_uuid_validation_accepts_non_nil_without_invented_version_constraints() {
    let mut bytes = fixture(1, &[0], 1);
    bytes[392..424].fill(0);
    bytes[407] = 1;
    bytes[423] = 1;
    let disk = open(bytes).unwrap();
    assert!(!disk.has_parent());
    disk.read_exact_at(0, &mut [0]).unwrap();
}
