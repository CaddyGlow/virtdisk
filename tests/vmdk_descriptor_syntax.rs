#![cfg(feature = "std")]
use std::fs;
use virtdisk::{ReadAt, Vmdk, VmdkWriter};

#[test]
fn colon_filename_is_rejected_before_creating_output() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk:1.vmdk");
    let source_path = directory.path().join("source.raw");
    fs::write(&source_path, [0; 512]).unwrap();
    let source = virtdisk::RawDisk::open(&source_path).unwrap();
    for mode in ["allocated", "sparse", "export"] {
        let result = match mode {
            "allocated" => VmdkWriter::create(&path, 512).map(drop),
            "sparse" => VmdkWriter::create_sparse(&path, 512).map(drop),
            "export" => virtdisk::create_vmdk(&path, &source),
            _ => unreachable!(),
        };
        assert_eq!(
            result.unwrap_err().kind(),
            virtdisk::io::ErrorKind::InvalidInput,
            "{mode}"
        );
        assert!(!path.try_exists().unwrap(), "{mode} left an output file");
    }
}

#[test]
fn library_created_filename_with_equals_reopens_as_hosted_chain() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk=1.vmdk");
    let writer = VmdkWriter::create(&path, 65536).unwrap();
    writer.write_all_at(0, &[7]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let disk = Vmdk::open_chain(&path, &[]).unwrap();
    let mut byte = [0];
    disk.read_exact_at(0, &mut byte).unwrap();
    assert_eq!(byte, [7]);
    drop(disk);
    drop(VmdkWriter::open(&path).unwrap());
}

#[test]
fn external_reader_and_writer_profiles_share_descriptor_syntax_checks() {
    for sparse in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let descriptor = directory.path().join("disk.vmdk");
        let extent = directory.path().join("extent with spaces=1.vmdk");
        let (profile, kind, tail) = if sparse {
            drop(VmdkWriter::create(&extent, 65536).unwrap());
            ("twoGbMaxExtentSparse", "SPARSE", "")
        } else {
            fs::write(&extent, vec![0; 65536]).unwrap();
            ("monolithicFlat", "FLAT", " 0")
        };
        let text = format!(
            "version = 1\r\n CID = 012a \r\nparentCID=ffffffff\r\ncreateType=\"{profile}\"\r\nRW 128 {kind} \"extent with spaces=1.vmdk\"{tail}\r\n"
        );
        fs::write(&descriptor, format!("{text}\0\0")).unwrap();
        let authorized = std::slice::from_ref(&extent);
        assert!(Vmdk::open_descriptor(&descriptor, authorized).is_ok());
        if sparse && !cfg!(target_os = "linux") {
            assert_eq!(
                VmdkWriter::open_descriptor(&descriptor, authorized)
                    .err()
                    .unwrap()
                    .kind(),
                virtdisk::io::ErrorKind::Unsupported
            );
        } else {
            drop(VmdkWriter::open_descriptor(&descriptor, authorized).unwrap());
        }
        for malformed in [
            format!("{text}CID=012a\n"),
            text.replace(
                "\"extent with spaces=1.vmdk\"",
                "\"extent with spaces=1.vmdk",
            ),
            format!("{text}version=1\n"),
            text.replace("parentCID=ffffffff", "parentCID=not-hex"),
            text.replace("RW 128", "RDONLY 128"),
            text.replace("RW 128", "RW 0"),
            text.replace(
                &format!("createType=\"{profile}\""),
                "createType=\"unsupported\"",
            ),
            format!("{text}\0CID=1234\n"),
        ] {
            fs::write(&descriptor, malformed).unwrap();
            assert!(Vmdk::open_descriptor(&descriptor, authorized).is_err());
            assert!(VmdkWriter::open_descriptor(&descriptor, authorized).is_err());
        }
    }
}

#[test]
fn hosted_chain_and_writer_reject_duplicate_cid_and_interior_padding() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vmdk");
    drop(VmdkWriter::create(&path, 65536).unwrap());
    let original = fs::read(&path).unwrap();
    let offset = u64::from_le_bytes(original[28..36].try_into().unwrap()) as usize * 512;
    let length = u64::from_le_bytes(original[36..44].try_into().unwrap()) as usize * 512;
    let end = original[offset..offset + length]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap();
    for extra in [&b"CID=1234\n"[..], &b"\0CID=1234\n"[..]] {
        let mut bytes = original.clone();
        bytes[offset + end..offset + end + extra.len()].copy_from_slice(extra);
        fs::write(&path, bytes).unwrap();
        assert!(Vmdk::open_chain(&path, &[]).is_err());
        assert!(VmdkWriter::open(&path).is_err());
    }
}
