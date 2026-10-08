use std::sync::Arc;
use virtdisk::{ParserLimits, Qcow2, ReadAt, Vdi, io};
struct Memory(Vec<u8>);
impl ReadAt for Memory {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(out.len() as u64)
            .filter(|end| *end <= self.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "memory bounds"))?;
        out.copy_from_slice(&self.0[offset as usize..end as usize]);
        Ok(())
    }
}
fn qcow(payload: &[u8], compressed: bool) -> Vec<u8> {
    let mut b = vec![0; 2560];
    b[..4].copy_from_slice(b"QFI\xfb");
    for (at, v) in [(4, 2u32), (20, 9), (36, 1), (56, 1)] {
        b[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
    for (at, v) in [(24, 512u64), (40, 512), (48, 1024), (512, 1536)] {
        b[at..at + 8].copy_from_slice(&v.to_be_bytes());
    }
    let entry = if compressed {
        (1u64 << 62) | 2048
    } else {
        2048
    };
    b[1536..1544].copy_from_slice(&entry.to_be_bytes());
    b[2048..2048 + payload.len()].copy_from_slice(payload);
    b
}
#[test]
fn portable_qcow_allocated_and_compressed_reads() {
    let data = vec![0x5a; 512];
    let compressed = miniz_oxide::deflate::compress_to_vec(&data, 6);
    for (payload, is_compressed) in [(&data, false), (&compressed, true)] {
        let disk = Qcow2::open(Arc::new(Memory(qcow(payload, is_compressed)))).unwrap();
        let mut out = [0; 512];
        disk.read_exact_at(0, &mut out).unwrap();
        assert_eq!(out.as_slice(), data);
        disk.read_exact_at(512, &mut []).unwrap();
        assert!(disk.read_exact_at(513, &mut []).is_err());
    }
}
#[test]
fn portable_qcow_rejects_invalid_compression_and_releases_scratch() {
    let disk = Qcow2::open_with_limits(
        Arc::new(Memory(qcow(&[0xff; 8], true))),
        ParserLimits::default(),
    )
    .unwrap();
    let budget = disk.budget().unwrap();
    for _ in 0..3 {
        assert!(disk.read_exact_at(0, &mut [0; 512]).is_err());
        assert!(budget.usage().cache_bytes <= 256);
    }
}
#[test]
fn portable_vdi_dynamic_zero_and_allocated_reads() {
    let mut b = vec![0; 1536];
    for (at, v) in [
        (64, 0xbeda107fu32),
        (68, 0x10001),
        (72, 400),
        (76, 1),
        (340, 512),
        (344, 1024),
        (360, 512),
        (376, 512),
        (384, 1),
        (388, 1),
    ] {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    b[368..376].copy_from_slice(&512u64.to_le_bytes());
    b[392] = 1;
    b[408] = 2;
    b[1024..].fill(0x6a);
    let disk = Vdi::open(Arc::new(Memory(b))).unwrap();
    let mut out = [0; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0x6a; 512]);
}

#[test]
fn portable_qcow_accounting_charges_repeated_uncached_reads() {
    let disk = Qcow2::open(Arc::new(Memory(qcow(&[0x5a; 512], false)))).unwrap();
    let budget = disk.budget().unwrap();
    disk.read_exact_at(0, &mut [0; 1]).unwrap();
    let first = budget.usage();
    disk.read_exact_at(0, &mut [0; 1]).unwrap();
    let second = budget.usage();
    #[cfg(not(feature = "std"))]
    {
        assert_eq!(second.metadata_bytes - first.metadata_bytes, 16);
        assert_eq!(second.cache_bytes, 0);
    }
    #[cfg(feature = "std")]
    {
        assert_eq!(second.metadata_bytes, first.metadata_bytes);
        assert_eq!(second.cache_bytes, first.cache_bytes);
    }
    assert!(second.work_items > first.work_items);
}

#[test]
fn portable_qcow_zstd_checks_exact_output() {
    let data = vec![0x5c; 512];
    // Single-segment, exact 512-byte frame with one raw block.
    let mut encoded = vec![0x28, 0xb5, 0x2f, 0xfd, 0x60, 0x00, 0x01, 0x01, 0x10, 0x00];
    encoded.extend_from_slice(&data);
    let mut image = qcow(&[], true);
    image.resize(3072, 0);
    image[2048..2048 + encoded.len()].copy_from_slice(&encoded);
    image[1536..1544].copy_from_slice(&((1u64 << 62) | (1u64 << 61) | 2048).to_be_bytes());
    image[4..8].copy_from_slice(&3u32.to_be_bytes());
    image[72..80].copy_from_slice(&8u64.to_be_bytes());
    image[96..100].copy_from_slice(&4u32.to_be_bytes());
    image[100..104].copy_from_slice(&112u32.to_be_bytes());
    image[104] = 1;
    let disk = Qcow2::open(Arc::new(Memory(image.clone()))).unwrap();
    let mut out = [0; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out.as_slice(), data);
    for (at, value) in [(2048, 0u8), (2053, 1), (2055, 0x09)] {
        let mut malformed = image.clone();
        malformed[at] = value;
        let disk = Qcow2::open(Arc::new(Memory(malformed))).unwrap();
        assert!(disk.read_exact_at(0, &mut out).is_err());
    }
}
#[test]
fn portable_deflate_rejects_expansion_and_accepts_sector_padding() {
    for (length, valid) in [(511, false), (512, true), (513, false), (4096, false)] {
        let payload = miniz_oxide::deflate::compress_to_vec(&vec![0x5a; length], 6);
        let disk = Qcow2::open(Arc::new(Memory(qcow(&payload, true)))).unwrap();
        assert_eq!(disk.read_exact_at(0, &mut [0; 512]).is_ok(), valid);
    }
}
struct Identified {
    bytes: Vec<u8>,
    token: virtdisk::SourceIdentity,
}
impl ReadAt for Identified {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn source_identity(&self) -> Option<virtdisk::SourceIdentity> {
        Some(self.token)
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(out.len() as u64)
            .filter(|end| *end <= self.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "bounds"))?;
        out.copy_from_slice(&self.bytes[offset as usize..end as usize]);
        Ok(())
    }
}
fn backing_child() -> Vec<u8> {
    let mut child = qcow(&[], false);
    child[512..520].fill(0);
    child[8..16].copy_from_slice(&128u64.to_be_bytes());
    child[16..20].copy_from_slice(&6u32.to_be_bytes());
    child[128..134].copy_from_slice(b"parent");
    child
}
#[test]
fn portable_qcow_supplied_parent_authority_and_identity() {
    let token = virtdisk::SourceIdentity::new(1, 1, 2560);
    let child = || {
        Arc::new(Identified {
            bytes: backing_child(),
            token,
        })
    };
    let parent = Arc::new(Identified {
        bytes: vec![0x73; 2560],
        token: virtdisk::SourceIdentity::new(1, 2, 2560),
    });
    let disk = Qcow2::open_with_raw_parent(child(), parent, ParserLimits::default()).unwrap();
    let mut out = [0; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0x73; 512]);
    let alias = Arc::new(Identified {
        bytes: vec![0; 2560],
        token,
    });
    assert!(Qcow2::open_with_raw_parent(child(), alias, ParserLimits::default()).is_err());
    let missing = Arc::new(Memory(vec![0; 2560]));
    assert_eq!(
        Qcow2::open_with_raw_parent(child(), missing, ParserLimits::default())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::MissingIdentity
    );
    let distinct_namespace = Arc::new(Identified {
        bytes: vec![0; 2560],
        token: virtdisk::SourceIdentity::new(2, 1, 2560),
    });
    assert!(
        Qcow2::open_with_raw_parent(child(), distinct_namespace, ParserLimits::default()).is_ok()
    );
}
fn vdi_bytes() -> Vec<u8> {
    let mut b = vec![0; 1536];
    for (at, v) in [
        (64, 0xbeda107fu32),
        (68, 0x10001),
        (72, 400),
        (76, 1),
        (340, 512),
        (344, 1024),
        (360, 512),
        (376, 512),
        (384, 1),
        (388, 1),
    ] {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    b[368..376].copy_from_slice(&512u64.to_le_bytes());
    b[392] = 1;
    b[408] = 2;
    b[1024..].fill(0x6a);
    b
}
#[test]
fn portable_vdi_parent_linkage_and_identity() {
    let parent = Arc::new(
        Vdi::open(Arc::new(Identified {
            bytes: vdi_bytes(),
            token: virtdisk::SourceIdentity::new(9, 1, 1536),
        }))
        .unwrap(),
    );
    let mut child = vdi_bytes();
    child[76..80].copy_from_slice(&4u32.to_le_bytes());
    child[392] = 3;
    child[408] = 4;
    child[424] = 1;
    child[440] = 2;
    child[512..516].copy_from_slice(&u32::MAX.to_le_bytes());
    child[388..392].fill(0);
    let source = Arc::new(Identified {
        bytes: child.clone(),
        token: virtdisk::SourceIdentity::new(9, 2, 1536),
    });
    let disk = Vdi::open_with_parent(source, parent.clone(), ParserLimits::default()).unwrap();
    let mut out = [0; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0x6a; 512]);
    child[440] = 8;
    assert!(
        Vdi::open_with_parent(
            Arc::new(Identified {
                bytes: child,
                token: virtdisk::SourceIdentity::new(9, 3, 1536)
            }),
            parent,
            ParserLimits::default()
        )
        .is_err()
    );
}
#[test]
fn portable_qcow_rejects_corrupt_header_and_mapping_words() {
    for (at, bytes) in [
        (4, 4u32.to_be_bytes().to_vec()),
        (20, 8u32.to_be_bytes().to_vec()),
        (32, 1u32.to_be_bytes().to_vec()),
        (512, 1u64.to_be_bytes().to_vec()),
        (1536, (2048u64 | 2).to_be_bytes().to_vec()),
    ] {
        let mut image = qcow(&[0x5a; 512], false);
        image[at..at + bytes.len()].copy_from_slice(&bytes);
        let result = Qcow2::open(Arc::new(Memory(image)))
            .and_then(|disk| disk.read_exact_at(0, &mut [0; 512]));
        assert!(result.is_err());
    }
}
#[test]
fn portable_vdi_rejects_nil_uuid_and_unowned_allocation() {
    for at in [64, 392, 408, 512] {
        let mut bytes = vdi_bytes();
        bytes[at..at + 4].fill(0);
        if at == 512 {
            bytes[512..516].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        assert!(Vdi::open(Arc::new(Memory(bytes))).is_err());
    }
}

#[test]
fn portable_qcow_supplied_parent_shares_cumulative_budget_and_rejects_new_limits() {
    let limits = ParserLimits {
        work_items: 64,
        ..ParserLimits::default()
    };
    let parent = Arc::new(
        Qcow2::open_with_limits(
            Arc::new(Identified {
                bytes: qcow(&[0x7b; 512], false),
                token: virtdisk::SourceIdentity::new(27, 1, 2560),
            }),
            limits,
        )
        .unwrap(),
    );
    let parent_budget = parent.budget().unwrap();
    let before = parent_budget.usage();
    let child_source = || {
        Arc::new(Identified {
            bytes: backing_child(),
            token: virtdisk::SourceIdentity::new(27, 2, 2560),
        })
    };
    let child = Qcow2::open_with_parent(child_source(), parent.clone(), limits).unwrap();
    assert!(parent_budget.usage().metadata_bytes > before.metadata_bytes);
    assert_eq!(child.budget().unwrap().usage(), parent_budget.usage());
    assert_eq!(
        Qcow2::open_with_parent(child_source(), parent, ParserLimits::default())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let remaining = limits.work_items - parent_budget.usage().work_items;
    parent_budget.work(remaining).unwrap();
    assert_eq!(
        child.read_exact_at(0, &mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::ResourceLimit
    );
    assert_eq!(parent_budget.usage().work_items, limits.work_items);
}

#[test]
fn portable_vdi_supplied_parent_shares_cumulative_budget_and_rejects_new_limits() {
    let limits = ParserLimits {
        work_items: 64,
        ..ParserLimits::default()
    };
    let parent = Arc::new(
        Vdi::open_with_limits(
            Arc::new(Identified {
                bytes: vdi_bytes(),
                token: virtdisk::SourceIdentity::new(28, 1, 1536),
            }),
            limits,
        )
        .unwrap(),
    );
    let parent_budget = parent.budget().unwrap();
    let before = parent_budget.usage();
    let mut child_bytes = vdi_bytes();
    child_bytes[76..80].copy_from_slice(&4u32.to_le_bytes());
    child_bytes[392] = 3;
    child_bytes[408] = 4;
    child_bytes[424] = 1;
    child_bytes[440] = 2;
    child_bytes[512..516].copy_from_slice(&u32::MAX.to_le_bytes());
    child_bytes[388..392].fill(0);
    let child_source = || {
        Arc::new(Identified {
            bytes: child_bytes.clone(),
            token: virtdisk::SourceIdentity::new(28, 2, 1536),
        })
    };
    let child = Vdi::open_with_parent(child_source(), parent.clone(), limits).unwrap();
    assert!(parent_budget.usage().metadata_bytes > before.metadata_bytes);
    assert_eq!(child.budget().unwrap().usage(), parent_budget.usage());
    assert_eq!(
        Vdi::open_with_parent(child_source(), parent, ParserLimits::default())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let remaining = limits.work_items - parent_budget.usage().work_items;
    parent_budget.work(remaining).unwrap();
    assert_eq!(
        child.read_exact_at(0, &mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::ResourceLimit
    );
    assert_eq!(parent_budget.usage().work_items, limits.work_items);
}

fn zstd_image(encoded: &[u8]) -> Vec<u8> {
    let sectors = encoded.len().div_ceil(512);
    let mut image = qcow(&[], true);
    image.resize(2048 + sectors * 512, 0);
    image[2048..2048 + encoded.len()].copy_from_slice(encoded);
    image[1536..1544]
        .copy_from_slice(&((1u64 << 62) | (((sectors - 1) as u64) << 61) | 2048).to_be_bytes());
    image[4..8].copy_from_slice(&3u32.to_be_bytes());
    image[72..80].copy_from_slice(&8u64.to_be_bytes());
    image[96..100].copy_from_slice(&4u32.to_be_bytes());
    image[100..104].copy_from_slice(&112u32.to_be_bytes());
    image[104] = 1;
    image
}
#[test]
fn portable_zstd_validates_checksums_truncation_and_releases_decoder_storage() {
    let mut encoded = vec![0x28, 0xb5, 0x2f, 0xfd, 0x64, 0, 1, 1, 0x10, 0];
    encoded.extend_from_slice(&[0x5c; 512]);
    // XXH64(seed=0) low 32 bits over the raw block.
    encoded.extend_from_slice(&0xd82a3a19u32.to_le_bytes());
    let disk = Qcow2::open(Arc::new(Memory(zstd_image(&encoded)))).unwrap();
    let mut output = [0; 512];
    disk.read_exact_at(0, &mut output).unwrap();
    assert_eq!(output, [0x5c; 512]);
    let mut corrupt_checksum = encoded.clone();
    *corrupt_checksum.last_mut().unwrap() ^= 1;
    for corrupt in [corrupt_checksum, encoded[..10].to_vec()] {
        let disk = Qcow2::open(Arc::new(Memory(zstd_image(&corrupt)))).unwrap();
        assert!(disk.read_exact_at(0, &mut output).is_err());
        // Only bounded mapping-cache leases can remain after a failed decode.
        assert!(disk.budget().unwrap().usage().cache_bytes <= 256);
    }
}
#[test]
fn portable_zstd_refuses_expansion_before_internal_growth() {
    let bodies = [
        // RLE literals claim 4096 regenerated bytes in a 512-byte-window frame.
        vec![0x0d, 0, 1, 0x5c, 0],
        // One sequence copies one literal followed by a 515-byte match.
        vec![8, 0x5c, 1, 0x54, 1, 0, 45, 0, 2],
        // An excessive sequence count must be refused before Vec::reserve.
        vec![0, 255, 255, 255, 0x54],
    ];
    for body in bodies {
        let mut encoded = vec![0x28, 0xb5, 0x2f, 0xfd, 0x60, 0, 1];
        let header = ((body.len() as u32) << 3) | 5;
        encoded.extend_from_slice(&header.to_le_bytes()[..3]);
        encoded.extend_from_slice(&body);
        let disk = Qcow2::open(Arc::new(Memory(zstd_image(&encoded)))).unwrap();
        assert!(disk.read_exact_at(0, &mut [0; 512]).is_err());
        assert!(disk.budget().unwrap().usage().cache_bytes <= 256);
    }
    let encoded = [0x28, 0xb5, 0x2f, 0xfd, 0x60, 0, 1, 3, 0, 16, 0x5c];
    let disk = Qcow2::open(Arc::new(Memory(zstd_image(&encoded)))).unwrap();
    assert!(disk.read_exact_at(0, &mut [0; 512]).is_err());
}
#[test]
fn portable_zstd_reserves_complete_decoder_storage_before_reading_payload() {
    let encoded = [0x28, 0xb5, 0x2f, 0xfd, 0x60, 0, 1, 3, 0x10, 0, 0x5c];
    let disk = Qcow2::open_with_limits(
        Arc::new(Memory(zstd_image(&encoded))),
        ParserLimits {
            cache_bytes: 32 * 1024,
            ..ParserLimits::default()
        },
    )
    .unwrap();
    assert_eq!(
        disk.read_exact_at(0, &mut [0; 512]).unwrap_err().kind(),
        io::ErrorKind::ResourceLimit
    );
    assert!(disk.budget().unwrap().usage().cache_bytes <= 256);
}
