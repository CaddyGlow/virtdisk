#![cfg(feature = "std")]
use std::{path::PathBuf, sync::Arc};
use virtdisk::{ExtentKind, InspectImage, RawDisk, ReadAt, Vdi};
fn overlay(path: &std::path::Path, parent: &std::path::Path) {
    let mut header = std::fs::read(parent).unwrap();
    header.resize(1024, 0);
    header[76..80].copy_from_slice(&4u32.to_le_bytes());
    header[388..392].copy_from_slice(&0u32.to_le_bytes());
    let creation: [u8; 16] = header[392..408].try_into().unwrap();
    let modification: [u8; 16] = header[408..424].try_into().unwrap();
    header[424..440].copy_from_slice(&creation);
    header[440..456].copy_from_slice(&modification);
    header[392..408].fill(31);
    header[408..424].fill(41);
    header[512..516].copy_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(path, header).unwrap();
}
#[test]
fn vdi_chain_inherits_free_blocks_but_explicit_zero_masks_parent() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    let writer = virtdisk::VdiWriter::create(&parent, 1048576).unwrap();
    writer.write_all_at(10, &[7; 16]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    overlay(&child, &parent);
    assert!(Vdi::open(Arc::new(RawDisk::open(&child).unwrap())).is_err());
    assert!(Vdi::open_chain(&child, &[]).is_err());
    let reader = Vdi::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    let mut bytes = [0; 16];
    reader.read_exact_at(10, &mut bytes).unwrap();
    assert_eq!(bytes, [7; 16]);
    assert!(reader.inspection().has_parent);
    let mut kinds = vec![];
    reader
        .visit_extents(&mut |extent| {
            kinds.push(extent.kind);
            Ok(())
        })
        .unwrap();
    assert_eq!(kinds, vec![ExtentKind::Inherited]);
    drop(reader);
    let mut file = std::fs::read(&child).unwrap();
    file[512..516].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
    std::fs::write(&child, file).unwrap();
    let reader = Vdi::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    reader.read_exact_at(10, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 16]);
}
#[test]
fn vdi_chain_rejects_wrong_parent_epochs_aliases_capacity_and_unneeded_ancestors() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    drop(virtdisk::VdiWriter::create(&parent, 1048576).unwrap());
    overlay(&child, &parent);
    let unrelated = directory.path().join("other.vdi");
    drop(virtdisk::VdiWriter::create(&unrelated, 1048576).unwrap());
    assert!(Vdi::open_chain(&child, std::slice::from_ref(&unrelated)).is_err());
    assert!(Vdi::open_chain(&child, &[parent.clone(), unrelated]).is_err());
    let alias = directory.path().join("alias.vdi");
    std::fs::hard_link(&child, &alias).unwrap();
    assert!(Vdi::open_chain(&child, &[alias]).is_err());
    let writer = virtdisk::VdiWriter::open(&parent).unwrap();
    writer.write_all_at(0, &[1]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert!(Vdi::open_chain(&child, std::slice::from_ref(&parent)).is_err());
}
#[test]
fn native_overlay_creator_supports_deeper_authorized_vdi_chains() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.vdi");
    let first = directory.path().join("first.vdi");
    let nested = directory.path().join("nested.vdi");
    drop(virtdisk::VdiWriter::create(&base, 1048576).unwrap());
    virtdisk::create_vdi_overlay(&first, &base, &[]).unwrap();
    virtdisk::create_vdi_overlay(&nested, &first, std::slice::from_ref(&base)).unwrap();
    let reader = Vdi::open_chain(&nested, &[first.clone(), base]).unwrap();
    assert_eq!(reader.len(), 1048576);
    assert!(virtdisk::create_vdi_overlay(&nested, &first, &[]).is_err());
    assert!(Vdi::open_chain(&nested, &[PathBuf::from("nonexistent")]).is_err());
}

#[test]
fn writable_vdi_overlay_copies_inherited_block_and_zeroes_without_changing_parent() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    let base = virtdisk::VdiWriter::create(&parent, 2 * 1048576).unwrap();
    base.write_all_at(0, &vec![7; 2 * 1048576]).unwrap();
    base.flush().unwrap();
    drop(base);
    let original = std::fs::read(&parent).unwrap();
    virtdisk::create_vdi_overlay(&child, &parent, &[]).unwrap();
    assert!(virtdisk::VdiWriter::open(&child).is_err());
    let writer = virtdisk::VdiWriter::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(100, &[9; 20]).unwrap();
    writer.write_zeroes(1048576 + 100, 20).unwrap();
    writer.flush().unwrap();
    let mut bytes = vec![0; 2 * 1048576];
    writer.read_exact_at(0, &mut bytes).unwrap();
    let mut expected = vec![7; 2 * 1048576];
    expected[100..120].fill(9);
    expected[1048576 + 100..1048576 + 120].fill(0);
    assert_eq!(bytes, expected);
    drop(writer);
    let reader = Vdi::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    reader.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(std::fs::read(parent).unwrap(), original);
}

#[test]
fn vdi_writer_overlay_creator_retains_lock_and_epoch_changes_only_child() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    drop(virtdisk::VdiWriter::create(&base, 1048576).unwrap());
    let writer = virtdisk::VdiWriter::create_overlay(&child, &base, &[]).unwrap();
    assert!(virtdisk::VdiWriter::open_chain(&child, std::slice::from_ref(&base)).is_err());
    let before = std::fs::read(&child).unwrap();
    writer.write_all_at(0, &[9]).unwrap();
    writer.flush().unwrap();
    let after = std::fs::read(&child).unwrap();
    assert_ne!(&before[408..424], &after[408..424]);
    assert_eq!(&before[424..456], &after[424..456]);
}

#[test]
fn unsupported_overlay_zero_allocation_fails_before_uuid_or_payload_mutation() {
    use virtdisk::io::Write;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    drop(virtdisk::VdiWriter::create(&base, 1048576).unwrap());
    virtdisk::create_vdi_overlay(&child, &base, &[]).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&child)
        .unwrap()
        .write_all(&[1])
        .unwrap();
    let before = std::fs::read(&child).unwrap();
    let writer = virtdisk::VdiWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    assert!(writer.write_zeroes(10, 5).is_err());
    assert_eq!(std::fs::read(child).unwrap(), before);
}

#[test]
fn vdi_chains_share_tightened_work_and_depth_limits() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    drop(virtdisk::VdiWriter::create(&base, 1048576).unwrap());
    virtdisk::create_vdi_overlay(&child, &base, &[]).unwrap();
    let limits = virtdisk::ParserLimits {
        recursion_depth: 1,
        ..Default::default()
    };
    assert!(Vdi::open_chain_with_limits(&child, std::slice::from_ref(&base), limits).is_err());
    let limits = virtdisk::ParserLimits {
        work_items: 4,
        ..Default::default()
    };
    assert!(Vdi::open_chain_with_limits(&child, std::slice::from_ref(&base), limits).is_err());
    let limits = virtdisk::ParserLimits {
        metadata_bytes: 500,
        ..Default::default()
    };
    assert!(Vdi::open_chain_with_limits(&child, std::slice::from_ref(&base), limits).is_err());
}

#[test]
fn vdi_differencing_can_inherit_from_different_parent_allocation_unit() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    drop(virtdisk::VdiWriter::create(&base, 1024).unwrap());
    let mut bytes = std::fs::read(&base).unwrap();
    bytes[376..380].copy_from_slice(&512u32.to_le_bytes());
    bytes[384..388].copy_from_slice(&2u32.to_le_bytes());
    bytes[388..392].copy_from_slice(&2u32.to_le_bytes());
    bytes[512..516].copy_from_slice(&0u32.to_le_bytes());
    bytes[516..520].copy_from_slice(&1u32.to_le_bytes());
    bytes[1024..2048].fill(8);
    bytes.truncate(2048);
    std::fs::write(&base, bytes).unwrap();
    virtdisk::create_vdi_overlay(&child, &base, &[]).unwrap();
    let reader = Vdi::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    let mut actual = [0; 1024];
    reader.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, [8; 1024]);
}

#[test]
fn vdi_overlay_depth_limit_is_checked_before_creating_destination() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.vdi");
    drop(virtdisk::VdiWriter::create(&base, 512).unwrap());
    let mut parent = base;
    let mut ancestors = vec![];
    for index in 0..31 {
        let child = directory.path().join(format!("child-{index}.vdi"));
        virtdisk::create_vdi_overlay(&child, &parent, &ancestors).unwrap();
        ancestors.insert(0, parent);
        parent = child;
    }
    let output = directory.path().join("too-deep.vdi");
    assert!(virtdisk::create_vdi_overlay(&output, &parent, &ancestors).is_err());
    assert!(!output.exists());
}

#[test]
fn native_chain_rejects_nil_child_ids_and_matching_nil_parent_modification() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    drop(virtdisk::VdiWriter::create(&parent, 1048576).unwrap());
    virtdisk::create_vdi_overlay(&child, &parent, &[]).unwrap();
    let original_parent = std::fs::read(&parent).unwrap();
    let original_child = std::fs::read(&child).unwrap();
    for at in [392, 408] {
        let mut bytes = original_child.clone();
        bytes[at..at + 16].fill(0);
        std::fs::write(&child, &bytes).unwrap();
        let error = Vdi::open_chain(&child, std::slice::from_ref(&parent))
            .err()
            .expect("nil child identity");
        assert_eq!(error.kind(), virtdisk::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&child).unwrap(), bytes);
    }
    let mut parent_bytes = original_parent;
    parent_bytes[408..424].fill(0);
    std::fs::write(&parent, &parent_bytes).unwrap();
    let mut child_bytes = original_child;
    child_bytes[440..456].fill(0);
    std::fs::write(&child, &child_bytes).unwrap();
    let error = Vdi::open_chain(&child, std::slice::from_ref(&parent))
        .err()
        .expect("matching nil modification epochs are invalid native headers");
    assert_eq!(error.kind(), virtdisk::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read(&parent).unwrap(), parent_bytes);
    assert_eq!(std::fs::read(&child).unwrap(), child_bytes);
}
