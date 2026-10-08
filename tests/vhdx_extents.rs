#![cfg(feature = "std")]
use std::sync::Arc;
use virtdisk::io;
use virtdisk::{
    Capability, DiskExtent, ExtentKind, ImageOperation, InspectImage, RawDisk, ReadAt, Vhdx,
    VhdxWriter,
};
const M: u64 = 1 << 20;
#[test]
fn coalesces_ordered_allocated_and_zero_runs_and_clips_final_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    let writer = VhdxWriter::create(&path, 4 * M + 512).unwrap();
    for offset in [0, M, 4 * M] {
        writer.write_all_at(offset, &[7]).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);
    let disk = Vhdx::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    let mut extents = Vec::new();
    disk.visit_extents(&mut |extent| {
        extents.push(extent);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        extents,
        vec![
            DiskExtent {
                offset: 0,
                length: 2 * M,
                kind: ExtentKind::Allocated
            },
            DiskExtent {
                offset: 2 * M,
                length: 2 * M,
                kind: ExtentKind::Zero
            },
            DiskExtent {
                offset: 4 * M,
                length: 512,
                kind: ExtentKind::Allocated
            }
        ]
    );
    assert_eq!(
        disk.inspection()
            .capabilities
            .get(ImageOperation::ExtentMap),
        Capability::Supported
    );
}
#[test]
fn visitor_cancellation_stops_work_at_the_first_run_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    let writer = VhdxWriter::create(&path, 5 * M).unwrap();
    writer.write_all_at(0, &[7]).unwrap();
    writer.write_all_at(M, &[7]).unwrap();
    drop(writer);
    let disk = Vhdx::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    let budget = disk.budget().unwrap();
    let before = budget.usage().work_items;
    let mut calls = 0;
    let err = disk
        .visit_extents(&mut |extent| {
            calls += 1;
            assert_eq!(extent.length, 2 * M);
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancel map"))
        })
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    assert_eq!(calls, 1);
    assert_eq!(budget.usage().work_items - before, 3);
}
#[test]
fn mapping_charges_deferred_work_and_does_not_read_payloads() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Guard {
        source: RawDisk,
        opened: AtomicBool,
    }
    impl ReadAt for Guard {
        fn len(&self) -> u64 {
            self.source.len()
        }
        fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
            assert!(
                !self.opened.load(Ordering::SeqCst),
                "mapping must not read the container again"
            );
            self.source.read_exact_at(offset, out)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    drop(VhdxWriter::create(&path, 3 * M + 512).unwrap());
    let source = Arc::new(Guard {
        source: RawDisk::open(path).unwrap(),
        opened: AtomicBool::new(false),
    });
    let disk = Vhdx::open(source.clone()).unwrap();
    source.opened.store(true, Ordering::SeqCst);
    let budget = disk.budget().unwrap();
    let before = budget.usage().work_items;
    let mut extents = Vec::new();
    disk.visit_extents(&mut |extent| {
        extents.push(extent);
        Ok(())
    })
    .unwrap();
    assert_eq!(budget.usage().work_items - before, 4);
    assert_eq!(
        extents,
        vec![DiskExtent {
            offset: 0,
            length: 3 * M + 512,
            kind: ExtentKind::Zero
        }]
    );
    let remaining = budget.limits().work_items - budget.usage().work_items;
    budget.work(remaining).unwrap();
    assert!(
        disk.visit_extents(&mut |_| panic!("budget failure must precede visitor"))
            .is_err()
    );
}

#[test]
fn all_nonpresent_standalone_states_follow_the_readers_zero_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    drop(VhdxWriter::create(&path, 3 * M + 512).unwrap());
    let mut bytes = std::fs::read(&path).unwrap();
    for (index, state) in [0u64, 1, 2, 3].into_iter().enumerate() {
        let start = 2 * M as usize + index * 8;
        bytes[start..start + 8].copy_from_slice(&state.to_le_bytes());
    }
    std::fs::write(&path, bytes).unwrap();
    let disk = Vhdx::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
    let mut extents = Vec::new();
    disk.visit_extents(&mut |e| {
        extents.push(e);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        extents,
        vec![DiskExtent {
            offset: 0,
            length: 3 * M + 512,
            kind: ExtentKind::Zero
        }]
    );
    for index in 0..4 {
        let mut data = [1; 512];
        disk.read_exact_at(index * M, &mut data).unwrap();
        assert_eq!(data, [0; 512]);
    }
}
