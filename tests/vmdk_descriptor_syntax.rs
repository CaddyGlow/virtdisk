use std::fs;
use virtdisk::{Vmdk, VmdkWriter};

#[test]
fn external_reader_and_writer_profiles_share_descriptor_syntax_checks() {
    for sparse in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let descriptor = directory.path().join("disk.vmdk");
        let extent = directory.path().join("extent with spaces.vmdk");
        let (profile, kind, tail) = if sparse {
            drop(VmdkWriter::create_sparse(&extent, 65536).unwrap());
            ("twoGbMaxExtentSparse", "SPARSE", "")
        } else {
            fs::write(&extent, vec![0; 65536]).unwrap();
            ("monolithicFlat", "FLAT", " 0")
        };
        let text = format!(
            "version = 1\r\n CID = 012a \r\nparentCID=ffffffff\r\ncreateType=\"{profile}\"\r\nRW 128 {kind} \"extent with spaces.vmdk\"{tail}\r\n"
        );
        fs::write(&descriptor, format!("{text}\0\0")).unwrap();
        let authorized = std::slice::from_ref(&extent);
        assert!(Vmdk::open_descriptor(&descriptor, authorized).is_ok());
        drop(VmdkWriter::open_descriptor(&descriptor, authorized).unwrap());
        for malformed in [
            format!("{text}CID=012a\n"),
            text.replace("\"extent with spaces.vmdk\"", "\"extent with spaces.vmdk"),
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
    drop(VmdkWriter::create_sparse(&path, 65536).unwrap());
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
