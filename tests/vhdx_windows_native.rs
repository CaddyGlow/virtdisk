use std::{io::Read, path::Path};
use virtdisk::{InspectImage, ReadAt, Vhdx};

fn unpack(bytes: &[u8], path: &Path) {
    let mut output = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take((16 << 20) + 1)
        .read_to_end(&mut output)
        .unwrap();
    assert!(output.len() <= 16 << 20);
    std::fs::write(path, output).unwrap();
}

#[test]
fn microsoft_produced_partial_children_preserve_native_locator_and_all_sectors() {
    for (sector, parent, child) in [
        (
            512usize,
            include_bytes!("fixtures/vhdx/windows-11-26200/512-base.vhdx.gz").as_slice(),
            include_bytes!("fixtures/vhdx/windows-11-26200/512-child.vhdx.gz").as_slice(),
        ),
        (
            4096,
            include_bytes!("fixtures/vhdx/windows-11-26200/4096-base.vhdx.gz").as_slice(),
            include_bytes!("fixtures/vhdx/windows-11-26200/4096-child.vhdx.gz").as_slice(),
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let parent_path = directory.path().join("base.vhdx");
        let child_path = directory.path().join("child.vhdx");
        unpack(parent, &parent_path);
        unpack(child, &child_path);
        let before_parent = std::fs::read(&parent_path).unwrap();
        let before_child = std::fs::read(&child_path).unwrap();
        // These native fixtures contain two PARTIALLY_PRESENT payload blocks
        // and one FULLY_PRESENT sector bitmap, rather than promoted full blocks.
        let bat = 3 << 20;
        let entry = |index| {
            u64::from_le_bytes(
                before_child[bat + index * 8..bat + index * 8 + 8]
                    .try_into()
                    .unwrap(),
            )
        };
        assert_eq!(entry(0) & 7, 7);
        assert_eq!(entry(1) & 7, 7);
        let chunk_ratio = (1 << 23) * sector / (2 << 20);
        assert_eq!(entry(chunk_ratio) & 7, 6);
        // Neither the native absolute locator nor its alternate keys are edited.
        // Explicit parent identity/linkage authorization must permit relocation.
        let disk = Vhdx::open_chain(&child_path, std::slice::from_ref(&parent_path)).unwrap();
        assert_eq!(disk.len(), 4 << 20);
        let mut expected = vec![7; 4 << 20];
        for (offset, byte) in [(8192, 11), (1056768, 13), (3153920, 17)] {
            expected[offset..offset + sector].fill(byte);
        }
        let mut actual = vec![0; expected.len()];
        disk.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(std::fs::read(&parent_path).unwrap(), before_parent);
        assert_eq!(std::fs::read(&child_path).unwrap(), before_child);
        assert!(Vhdx::open_chain(&child_path, &[]).is_err());
    }
}

#[test]
fn windows_terminated_child_replays_exactly_without_mutating_unauthorized_sources() {
    for (sector, parent, child) in [
        (
            512,
            include_bytes!("fixtures/vhdx/windows-process-kill/512-base.vhdx.gz").as_slice(),
            include_bytes!("fixtures/vhdx/windows-process-kill/512-child.vhdx.gz").as_slice(),
        ),
        (
            4096,
            include_bytes!("fixtures/vhdx/windows-process-kill/4096-base.vhdx.gz").as_slice(),
            include_bytes!("fixtures/vhdx/windows-process-kill/4096-child.vhdx.gz").as_slice(),
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let parent_path = directory.path().join("base.vhdx");
        let child_path = directory.path().join("child.vhdx");
        unpack(parent, &parent_path);
        unpack(child, &child_path);
        let before_parent = std::fs::read(&parent_path).unwrap();
        let before_child = std::fs::read(&child_path).unwrap();
        assert!(Vhdx::open_chain(&child_path, std::slice::from_ref(&parent_path)).is_err());
        assert!(Vhdx::open_recovered_chain(&child_path, &[]).is_err());
        assert!(virtdisk::recover_vhdx_chain(&child_path, &[]).is_err());
        assert_eq!(std::fs::read(&child_path).unwrap(), before_child);
        assert_eq!(std::fs::read(&parent_path).unwrap(), before_parent);

        let mut expected = vec![7; 4 << 20];
        expected[8195..8203].fill(11);
        let mut actual = vec![0; expected.len()];
        let reader =
            Vhdx::open_recovered_chain(&child_path, std::slice::from_ref(&parent_path)).unwrap();
        assert_eq!(
            reader.inspection().geometry.logical_sector_size,
            Some(sector)
        );
        reader.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
        drop(reader);
        assert_eq!(std::fs::read(&child_path).unwrap(), before_child);
        assert_eq!(std::fs::read(&parent_path).unwrap(), before_parent);

        virtdisk::recover_vhdx_chain(&child_path, std::slice::from_ref(&parent_path)).unwrap();
        let recovered = std::fs::read(&child_path).unwrap();
        assert_ne!(recovered, before_child);
        let reader = Vhdx::open_chain(&child_path, std::slice::from_ref(&parent_path)).unwrap();
        reader.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
        drop(reader);
        virtdisk::recover_vhdx_chain(&child_path, std::slice::from_ref(&parent_path)).unwrap();
        assert_eq!(std::fs::read(&child_path).unwrap(), recovered);
        assert_eq!(std::fs::read(&parent_path).unwrap(), before_parent);
    }
}
