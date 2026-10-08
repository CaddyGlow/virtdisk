#![cfg(feature = "std")]
use std::{fs, sync::Arc};
use virtdisk::io;
use virtdisk::{
    CheckOptions, CheckScope, ImageFormat, RawDisk, ReadAt, check_image, check_image_with_cancel,
    check_payload_with_cancel, convert_image, create_qcow2_overlay,
};

#[test]
fn checks_every_container_without_changing_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    fs::write(&raw, vec![37; 131072]).unwrap();
    let source = RawDisk::open(&raw).unwrap();
    for (index, format) in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("disk{index}"));
        convert_image(&source, &path, format).unwrap();
        let before = fs::read(&path).unwrap();
        let report = check_image(&path, format, &[], CheckOptions { payload: true }).unwrap();
        assert_eq!(report.virtual_size, 131072);
        assert_eq!(report.payload_bytes_read, 131072);
        assert_eq!(
            report.structural,
            if format == ImageFormat::Raw {
                CheckScope::RawLengthOnly
            } else {
                CheckScope::SupportedContainerOwnership
            }
        );
        assert_eq!(fs::read(path).unwrap(), before);
    }
}

#[test]
fn qcow_check_rejects_bad_refcount_that_open_alone_accepts() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    fs::write(&raw, vec![31; 65536]).unwrap();
    let path = directory.path().join("qcow");
    convert_image(&RawDisk::open(raw).unwrap(), &path, ImageFormat::Qcow2).unwrap();
    let mut bytes = fs::read(&path).unwrap();
    let table = u64::from_be_bytes(bytes[48..56].try_into().unwrap()) as usize;
    let block = u64::from_be_bytes(bytes[table..table + 8].try_into().unwrap()) as usize;
    bytes[block..block + 2].fill(0);
    fs::write(&path, &bytes).unwrap();
    virtdisk::Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    assert_eq!(
        check_image(&path, ImageFormat::Qcow2, &[], CheckOptions::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn check_chain_requires_authorization_and_cancel_does_not_mutate() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent");
    fs::write(&parent, vec![19; 131072]).unwrap();
    let child = directory.path().join("child");
    create_qcow2_overlay(&child, &parent, "raw", 131072).unwrap();
    let before = fs::read(&child).unwrap();
    assert!(check_image(&child, ImageFormat::Qcow2, &[], CheckOptions::default()).is_err());
    assert_eq!(
        check_image(
            &child,
            ImageFormat::Qcow2,
            std::slice::from_ref(&parent),
            CheckOptions { payload: true }
        )
        .unwrap()
        .payload_bytes_read,
        131072
    );
    assert_eq!(
        check_image_with_cancel(
            &child,
            ImageFormat::Qcow2,
            &[parent],
            CheckOptions { payload: true },
            || true
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(fs::read(child).unwrap(), before);
}

#[test]
fn qcow_check_audits_parent_ownership_without_sweeping_by_default() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    fs::write(&raw, vec![23; 65536]).unwrap();
    let parent = directory.path().join("parent");
    convert_image(&RawDisk::open(raw).unwrap(), &parent, ImageFormat::Qcow2).unwrap();
    let child = directory.path().join("child");
    create_qcow2_overlay(&child, &parent, "qcow2", 65536).unwrap();
    let report = check_image(
        &child,
        ImageFormat::Qcow2,
        std::slice::from_ref(&parent),
        CheckOptions::default(),
    )
    .unwrap();
    assert_eq!(report.payload_bytes_read, 0);
    let mut bytes = fs::read(&parent).unwrap();
    let table = u64::from_be_bytes(bytes[48..56].try_into().unwrap()) as usize;
    let block = u64::from_be_bytes(bytes[table..table + 8].try_into().unwrap()) as usize;
    bytes[block..block + 2].fill(0);
    fs::write(&parent, &bytes).unwrap();
    assert_eq!(
        check_image(
            &child,
            ImageFormat::Qcow2,
            &[parent],
            CheckOptions::default()
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
}

struct FailingPayload;
impl ReadAt for FailingPayload {
    fn len(&self) -> u64 {
        131072
    }
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        if offset >= 65536 {
            return Err(virtdisk::ReadContext {
                container: Some("authorized-parent".into()),
                offset: Some(offset),
                ..Default::default()
            }
            .error(
                "read parent payload",
                io::Error::new(io::ErrorKind::UnexpectedEof, "payload unavailable"),
            ));
        }
        destination.fill(0);
        Ok(())
    }
}

#[test]
fn payload_sweep_preserves_read_errors_and_checks_cancel_between_chunks() {
    let error = check_payload_with_cancel(&FailingPayload, || false).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    let detail = error
        .get_ref()
        .unwrap()
        .downcast_ref::<virtdisk::ReadError>()
        .unwrap();
    assert_eq!(detail.operation, "read parent payload");
    assert_eq!(detail.context.offset, Some(65536));
    assert_eq!(
        detail.context.container.as_deref(),
        Some("authorized-parent")
    );
    let mut calls = 0;
    assert_eq!(
        check_payload_with_cancel(&FailingPayload, || {
            calls += 1;
            calls == 2
        })
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(calls, 2);
}

#[test]
fn supported_owner_checks_reject_vdi_vmdk_and_vhdx_payload_aliases() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    fs::write(&raw, vec![29; 2097152]).unwrap();
    let source = RawDisk::open(raw).unwrap();
    for (index, format) in [ImageFormat::Vdi, ImageFormat::Vmdk, ImageFormat::Vhdx]
        .into_iter()
        .enumerate()
    {
        let path = directory.path().join(format!("disk{index}"));
        convert_image(&source, &path, format).unwrap();
        let original = fs::read(&path).unwrap();
        let mut aliased = original.clone();
        match format {
            ImageFormat::Vdi => {
                let map = u32::from_le_bytes(aliased[340..344].try_into().unwrap()) as usize;
                let first = aliased[map..map + 4].to_vec();
                aliased[map + 4..map + 8].copy_from_slice(&first);
            }
            ImageFormat::Vmdk => {
                let gd = u64::from_le_bytes(aliased[56..64].try_into().unwrap()) as usize * 512;
                let table =
                    u32::from_le_bytes(aliased[gd..gd + 4].try_into().unwrap()) as usize * 512;
                let first = aliased[table..table + 4].to_vec();
                aliased[table + 4..table + 8].copy_from_slice(&first);
            }
            ImageFormat::Vhdx => {
                let bat = vhdx_region(&aliased, false);
                let first = aliased[bat..bat + 8].to_vec();
                aliased[bat + 8..bat + 16].copy_from_slice(&first);
            }
            _ => unreachable!(),
        }
        fs::write(&path, &aliased).unwrap();
        assert_eq!(
            check_image(&path, format, &[], CheckOptions::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&path).unwrap(), aliased);
        // A declared payload location inside protected metadata is also invalid.
        let mut overlap = original;
        match format {
            ImageFormat::Vdi => {
                overlap[344..348].copy_from_slice(&512u32.to_le_bytes());
            }
            ImageFormat::Vmdk => {
                let gd = u64::from_le_bytes(overlap[56..64].try_into().unwrap()) as usize * 512;
                let table =
                    u32::from_le_bytes(overlap[gd..gd + 4].try_into().unwrap()) as usize * 512;
                overlap[table..table + 4].copy_from_slice(&2u32.to_le_bytes());
            }
            ImageFormat::Vhdx => {
                let bat = vhdx_region(&overlap, false);
                let metadata = vhdx_region(&overlap, true) as u64;
                overlap[bat..bat + 8].copy_from_slice(&(metadata | 6).to_le_bytes());
            }
            _ => unreachable!(),
        }
        fs::write(&path, &overlap).unwrap();
        assert_eq!(
            check_image(&path, format, &[], CheckOptions::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(path).unwrap(), overlap);
    }
}

fn vhdx_region(bytes: &[u8], metadata: bool) -> usize {
    let table = &bytes[192 * 1024..256 * 1024];
    let expected_first_guid_byte = if metadata { 6 } else { 0x66 };
    table[16..16 + u32::from_le_bytes(table[8..12].try_into().unwrap()) as usize * 32]
        .as_chunks::<32>()
        .0
        .iter()
        .find(|entry| entry[0] == expected_first_guid_byte)
        .map(|entry| u64::from_le_bytes(entry[16..24].try_into().unwrap()) as usize)
        .unwrap()
}

#[test]
fn vhdx_optional_metadata_is_range_checked_but_contents_are_opaque() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    fs::write(&raw, vec![0; 512]).unwrap();
    let path = directory.path().join("vhdx");
    convert_image(&RawDisk::open(raw).unwrap(), &path, ImageFormat::Vhdx).unwrap();
    let mut bytes = fs::read(&path).unwrap();
    let metadata = vhdx_region(&bytes, true);
    let count =
        u16::from_le_bytes(bytes[metadata + 10..metadata + 12].try_into().unwrap()) as usize;
    let entry = metadata + 32 + count * 32;
    bytes[metadata + 10..metadata + 12].copy_from_slice(&((count + 1) as u16).to_le_bytes());
    bytes[entry..entry + 16].fill(0xa5);
    bytes[entry + 16..entry + 20].copy_from_slice(&70000u32.to_le_bytes());
    bytes[entry + 20..entry + 24].copy_from_slice(&16u32.to_le_bytes());
    bytes[metadata + 70000..metadata + 70016].fill(0xff);
    fs::write(&path, &bytes).unwrap();
    let report = check_image(&path, ImageFormat::Vhdx, &[], CheckOptions::default()).unwrap();
    assert_eq!(report.structural, CheckScope::SupportedContainerOwnership);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    bytes[entry + 16..entry + 20].copy_from_slice(&65536u32.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        check_image(&path, ImageFormat::Vhdx, &[], CheckOptions::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}
