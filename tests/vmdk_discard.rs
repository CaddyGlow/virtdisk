#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use std::{fs, sync::Arc};
use virtdisk::io;
use virtdisk::{DiscardPolicy, DiscardResult, RawDisk, ReadAt, Vmdk, VmdkWriter};

#[test]
fn complete_and_clipped_grains_release_mappings_without_changing_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let writer = VmdkWriter::create(&path, 131584).unwrap();
    writer.write_all_at(0, &[17; 512]).unwrap();
    writer.write_all_at(65536, &[31; 512]).unwrap();
    writer.write_all_at(131072, &[47; 512]).unwrap();
    let physical_length = fs::metadata(&path).unwrap().len();
    assert_eq!(
        writer
            .discard(65536, 66048, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    assert_eq!(writer.len(), 131584);
    assert_eq!(fs::metadata(&path).unwrap().len(), physical_length);
    let mut actual = vec![1; 131584];
    writer.read_exact_at(0, &mut actual).unwrap();
    let mut expected = vec![0; 131584];
    expected[..512].fill(17);
    assert_eq!(actual, expected);
    writer
        .discard(0, 65536, DiscardPolicy::RequireDeallocation)
        .unwrap();
    writer.read_exact_at(0, &mut actual).unwrap();
    assert!(actual[..65536].iter().all(|byte| *byte == 0));
    writer.write_all_at(17, &[71; 32]).unwrap();
    writer
        .discard(0, 65536, DiscardPolicy::RequireDeallocation)
        .unwrap();
    writer.write_all_at(17, &[71; 32]).unwrap();
    expected[..512].fill(0);
    expected[17..49].fill(71);
    writer.flush().unwrap();
    drop(writer);
    Vmdk::open(Arc::new(RawDisk::open(&path).unwrap()))
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn inherited_grain_discard_masks_parent_across_reopen_and_partial_rewrite() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent.vmdk");
    let base = VmdkWriter::create(&parent, 131584).unwrap();
    base.write_all_at(0, &vec![53; 131584]).unwrap();
    base.flush().unwrap();
    drop(base);
    let parent_bytes = fs::read(&parent).unwrap();
    let path = directory.path().join("child.vmdk");
    let writer = VmdkWriter::create_overlay(&path, &parent, std::slice::from_ref(&parent)).unwrap();
    assert_eq!(
        writer
            .discard(0, 65536, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    writer
        .discard(131072, 512, DiscardPolicy::RequireDeallocation)
        .unwrap();
    writer.flush().unwrap();
    drop(writer);
    let writer = VmdkWriter::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(17, &[71; 32]).unwrap();
    let mut actual = vec![0; 131584];
    writer.read_exact_at(0, &mut actual).unwrap();
    let mut expected = vec![0; 131584];
    expected[17..49].fill(71);
    expected[65536..131072].fill(53);
    assert_eq!(actual, expected);
    writer
        .discard(0, 65536, DiscardPolicy::RequireDeallocation)
        .unwrap();
    writer.read_exact_at(0, &mut actual).unwrap();
    assert!(actual[..65536].iter().all(|byte| *byte == 0));
    writer.write_all_at(17, &[71; 32]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    Vmdk::open_chain(&path, std::slice::from_ref(&parent))
        .unwrap()
        .read_exact_at(0, &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(fs::read(parent).unwrap(), parent_bytes);
}

#[test]
fn rejected_ranges_and_strict_partial_discard_leave_cid_and_bytes_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let writer = VmdkWriter::create(&path, 131072).unwrap();
    writer.write_all_at(0, &vec![23; 131072]).unwrap();
    writer.flush().unwrap();
    let before = fs::read(&path).unwrap();
    for (offset, length, kind) in [
        (1, 65536, io::ErrorKind::Unsupported),
        (0, 65535, io::ErrorKind::Unsupported),
        (65536, 65537, io::ErrorKind::UnexpectedEof),
        (u64::MAX, 1, io::ErrorKind::UnexpectedEof),
    ] {
        assert_eq!(
            writer
                .discard(offset, length, DiscardPolicy::RequireDeallocation)
                .unwrap_err()
                .kind(),
            kind
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert_eq!(
        writer
            .discard(17, 32, DiscardPolicy::AllowZeroFallback)
            .unwrap(),
        DiscardResult::Zeroed
    );
    let mut actual = [0; 64];
    writer.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(&actual[..17], &[23; 17]);
    assert_eq!(&actual[17..49], &[0; 32]);
    assert_eq!(&actual[49..], &[23; 15]);
}

#[test]
fn already_masked_grains_are_idempotent_and_flat_descriptor_strict_discard_refuses() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    let writer = VmdkWriter::create_sparse(&path, 65536).unwrap();
    writer
        .discard(0, 65536, DiscardPolicy::RequireDeallocation)
        .unwrap();
    writer.flush().unwrap();
    let before = fs::read(&path).unwrap();
    writer
        .discard(0, 65536, DiscardPolicy::RequireDeallocation)
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), before);
    let descriptor = directory.path().join("flat.vmdk");
    let extent = directory.path().join("extent");
    fs::write(&extent, vec![37; 65536]).unwrap();
    let descriptor_bytes = b"# Disk DescriptorFile\nversion=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\nRW 128 FLAT \"extent\" 0\n";
    fs::write(&descriptor, descriptor_bytes).unwrap();
    let writer = VmdkWriter::open_descriptor(&descriptor, std::slice::from_ref(&extent)).unwrap();
    assert_eq!(
        writer
            .discard(0, 65536, DiscardPolicy::RequireDeallocation)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fs::read(&descriptor).unwrap(), descriptor_bytes);
    assert_eq!(fs::read(&extent).unwrap(), vec![37; 65536]);
    assert_eq!(
        writer
            .discard(17, 32, DiscardPolicy::AllowZeroFallback)
            .unwrap(),
        DiscardResult::Zeroed
    );
    let mut expected = vec![37; 65536];
    expected[17..49].fill(0);
    assert_eq!(fs::read(extent).unwrap(), expected);
}

#[test]
fn unsupported_physical_journal_length_refuses_before_changing_cid() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    drop(VmdkWriter::create_sparse(&path, 65536).unwrap());
    let before = fs::read(&path).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(34 * 1024 * 1024 * 1024)
        .unwrap();
    let writer = VmdkWriter::open(&path).unwrap();
    assert_eq!(
        writer
            .discard(0, 65536, DiscardPolicy::RequireDeallocation)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    let mut prefix = vec![0; before.len()];
    RawDisk::open(&path)
        .unwrap()
        .read_exact_at(0, &mut prefix)
        .unwrap();
    assert_eq!(prefix, before);
    assert_eq!(fs::metadata(path).unwrap().len(), 34 * 1024 * 1024 * 1024);
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn native_redundant_zero_grains_mask_qemu_parent_and_preserve_rewritten_neighbors() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("base.raw");
    fs::write(&raw, vec![53; 131584]).unwrap();
    let parent = directory.path().join("parent.vmdk");
    let created = std::process::Command::new("qemu-img")
        .args(["convert", "-f", "raw", "-O", "vmdk"])
        .arg(&raw)
        .arg(&parent)
        .output()
        .unwrap();
    assert!(created.status.success());
    let parent_bytes = fs::read(&parent).unwrap();
    for overlay in [false, true] {
        let path = directory.path().join(if overlay {
            "child.vmdk"
        } else {
            "standalone.vmdk"
        });
        let mut command = std::process::Command::new("qemu-img");
        command.args(["create", "-f", "vmdk"]);
        if overlay {
            command.args(["-F", "vmdk", "-b"]).arg(&parent);
        }
        let created = command.arg(&path).arg("131584").output().unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
        let authorities = if overlay {
            vec![parent.clone()]
        } else {
            vec![]
        };
        let writer = if overlay {
            VmdkWriter::open_chain(&path, &authorities).unwrap()
        } else {
            VmdkWriter::open(&path).unwrap()
        };
        if !overlay {
            writer.write_all_at(0, &vec![53; 131584]).unwrap();
        }
        writer.write_all_at(0, &[19; 512]).unwrap();
        writer
            .discard(0, 65536, DiscardPolicy::RequireDeallocation)
            .unwrap();
        writer
            .discard(131072, 512, DiscardPolicy::RequireDeallocation)
            .unwrap();
        writer.write_all_at(17, &[71; 32]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let image = fs::read(&path).unwrap();
        assert_ne!(u32::from_le_bytes(image[8..12].try_into().unwrap()) & 4, 0);
        for offset in [48, 56] {
            let directory =
                u64::from_le_bytes(image[offset..offset + 8].try_into().unwrap()) as usize * 512;
            assert_ne!(directory, 0);
            let table = u32::from_le_bytes(image[directory..directory + 4].try_into().unwrap())
                as usize
                * 512;
            assert_eq!(
                u32::from_le_bytes(image[table + 8..table + 12].try_into().unwrap()),
                1
            );
        }
        let checked = std::process::Command::new("qemu-img")
            .args(["check", "-f", "vmdk"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        let output = directory.path().join(if overlay {
            "child.raw"
        } else {
            "standalone.raw"
        });
        let converted = std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vmdk", "-O", "raw"])
            .arg(&path)
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            converted.status.success(),
            "{}",
            String::from_utf8_lossy(&converted.stderr)
        );
        let mut expected = vec![0; 131584];
        expected[17..49].fill(71);
        expected[65536..131072].fill(53);
        assert_eq!(fs::read(output).unwrap(), expected);
        assert_eq!(fs::read(&parent).unwrap(), parent_bytes);
    }
}
