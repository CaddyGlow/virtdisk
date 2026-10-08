use std::sync::Arc;
use virtdisk::{ParserLimits, ReadAt, SourceIdentity, Vhdx, Vmdk, VmdkExtentBinding, io};
#[path = "support/vhdx.rs"]
mod vhdx_fixture;
struct Memory {
    bytes: Vec<u8>,
    identity: Option<SourceIdentity>,
}
impl ReadAt for Memory {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn source_identity(&self) -> Option<SourceIdentity> {
        self.identity
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(out.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "overflow"))?;
        if end > self.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "bounds"));
        }
        out.copy_from_slice(&self.bytes[offset as usize..end as usize]);
        Ok(())
    }
}
fn memory(bytes: Vec<u8>, storage: u128) -> Arc<dyn ReadAt> {
    let length = bytes.len() as u64;
    Arc::new(Memory {
        bytes,
        identity: Some(SourceIdentity::new(7, storage, length)),
    })
}
fn descriptor() -> Arc<dyn ReadAt> {
    memory(b"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\nRW 1 FLAT \"a\" 0\nRW 1 FLAT \"b\" 1\n".to_vec(), 1)
}
#[test]
fn portable_flat_bindings_read_and_reject_aliases() {
    let a = memory(vec![42; 512], 2);
    let b = memory(vec![17; 1024], 3);
    let bindings = [
        VmdkExtentBinding {
            name: "a".into(),
            source: a.clone(),
        },
        VmdkExtentBinding {
            name: "b".into(),
            source: b,
        },
    ];
    let disk = Vmdk::open_descriptor_bound(descriptor(), &bindings, None, ParserLimits::default())
        .unwrap();
    let mut out = [0; 4];
    disk.read_exact_at(510, &mut out).unwrap();
    assert_eq!(out, [42, 42, 17, 17]);
    let aliases = [
        VmdkExtentBinding {
            name: "a".into(),
            source: a.clone(),
        },
        VmdkExtentBinding {
            name: "b".into(),
            source: a,
        },
    ];
    assert!(
        Vmdk::open_descriptor_bound(descriptor(), &aliases, None, ParserLimits::default()).is_err()
    );
    assert!(
        Vmdk::open_descriptor_bound(descriptor(), &bindings[..1], None, ParserLimits::default())
            .is_err()
    );
}
#[test]
fn portable_descriptor_missing_identity_and_budget_are_rejected() {
    let source: Arc<dyn ReadAt> = Arc::new(Memory {
        bytes: vec![0; 1024],
        identity: None,
    });
    let bindings = [
        VmdkExtentBinding {
            name: "a".into(),
            source: source.clone(),
        },
        VmdkExtentBinding {
            name: "b".into(),
            source,
        },
    ];
    assert!(
        Vmdk::open_descriptor_bound(descriptor(), &bindings, None, ParserLimits::default())
            .is_err()
    );
    let limits = ParserLimits {
        metadata_bytes: 4,
        ..Default::default()
    };
    assert!(Vmdk::open_descriptor_bound(descriptor(), &bindings, None, limits).is_err());
}
#[test]
fn portable_vhdx_payload_corruption_bounds_and_recovery() {
    let bytes = vhdx_fixture::fixture();
    let disk = Vhdx::open(memory(bytes.clone(), 10)).unwrap();
    let mut out = [0; 4];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [17; 4]);
    assert!(disk.read_exact_at(disk.len(), &mut []).is_ok());
    assert!(disk.read_exact_at(disk.len() + 1, &mut []).is_err());
    let recovered = Vhdx::open_recovered(memory(bytes.clone(), 11)).unwrap();
    recovered.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [17; 4]);
    let mut corrupt = bytes;
    vhdx_fixture::put64(
        &mut corrupt,
        2 * vhdx_fixture::M,
        (3 * vhdx_fixture::M as u64) | 6,
    );
    assert!(Vhdx::open(memory(corrupt, 12)).is_err());
}
fn sparse() -> Vec<u8> {
    let mut bytes = vec![0; 2048];
    bytes[..4].copy_from_slice(b"KDMV");
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    for (offset, value) in [(12, 2u64), (20, 1), (56, 1), (64, 3)] {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    bytes[44..48].copy_from_slice(&2u32.to_le_bytes());
    bytes[512..516].copy_from_slice(&2u32.to_le_bytes());
    bytes[1024..1028].copy_from_slice(&3u32.to_le_bytes());
    bytes[1536..].fill(42);
    bytes
}
#[test]
fn portable_sparse_bindings_capacity_and_distinct_identical_sources() {
    let descriptor = memory(b"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 2 SPARSE \"a\"\nRW 2 SPARSE \"b\"\n".to_vec(),1);
    let bindings = [
        VmdkExtentBinding {
            name: "a".into(),
            source: memory(sparse(), 2),
        },
        VmdkExtentBinding {
            name: "b".into(),
            source: memory(sparse(), 3),
        },
    ];
    let disk =
        Vmdk::open_descriptor_bound(descriptor, &bindings, None, ParserLimits::default()).unwrap();
    let mut out = [0; 4];
    disk.read_exact_at(1024, &mut out).unwrap();
    assert_eq!(out, [42; 4]);
    let wrong = memory(b"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"monolithicSparse\"\nRW 1 SPARSE \"a\"\n".to_vec(),1);
    assert!(
        Vmdk::open_descriptor_bound(wrong, &bindings[..1], None, ParserLimits::default()).is_err()
    );
}
#[path = "support/vhdx_chain.rs"]
mod vhdx_chain;
mod fixture_support {
    pub use super::vhdx_fixture::*;
}
#[test]
fn portable_vhdx_supplied_parent_validates_identity_and_linkage() {
    let parent = Arc::new(Vhdx::open(memory(vhdx_fixture::fixture(), 10)).unwrap());
    let child = Vhdx::open_parented(memory(vhdx_chain::child(), 11), parent.clone()).unwrap();
    let mut out = [0; 4];
    child.read_exact_at(0, &mut out).unwrap();
    let alias: Arc<dyn ReadAt> = Arc::new(Memory {
        bytes: vhdx_chain::child(),
        identity: parent.source_identity(),
    });
    assert!(Vhdx::open_parented(alias, parent.clone()).is_err());
    let mut wrong = vhdx_fixture::fixture();
    wrong[65536 + 32] = 1;
    wrong[131072 + 32] = 1;
    vhdx_fixture::checksum(&mut wrong[65536..65536 + 4096]);
    vhdx_fixture::checksum(&mut wrong[131072..131072 + 4096]);
    let wrong = Arc::new(Vhdx::open(memory(wrong, 12)).unwrap());
    assert!(Vhdx::open_parented(memory(vhdx_chain::child(), 13), wrong).is_err());
}

const M: usize = vhdx_fixture::M;
use vhdx_fixture::{put, put64};
fn crc(bytes: &mut [u8]) {
    vhdx_fixture::checksum(bytes);
}
fn entry(data: Option<&[u8]>, seq: u64, tail: u32, target: u64, length: u64) -> Vec<u8> {
    let mut b = vec![0; if data.is_some() { 8192 } else { 4096 }];
    b[..4].copy_from_slice(b"loge");
    let size = b.len() as u32;
    put(&mut b, 8, size);
    put(&mut b, 12, tail);
    put64(&mut b, 16, seq);
    put(&mut b, 24, 1);
    b[32..48].fill(0x77);
    put64(&mut b, 48, 6 * M as u64);
    put64(&mut b, 56, length);
    put64(&mut b, 64 + 16, target);
    put64(&mut b, 64 + 24, seq);
    if let Some(data) = data {
        b[64..68].copy_from_slice(b"desc");
        b[68..72].copy_from_slice(&data[4092..4096]);
        b[72..80].copy_from_slice(&data[..8]);
        b[4096..4100].copy_from_slice(b"data");
        put(&mut b, 4100, (seq >> 32) as u32);
        b[4104..8188].copy_from_slice(&data[8..4092]);
        put(&mut b, 8188, seq as u32);
    } else {
        b[64..68].copy_from_slice(b"zero");
        put64(&mut b, 72, 4096);
    }
    crc(&mut b);
    b
}
fn dirty(data: bool) -> Vec<u8> {
    let mut b = vhdx_fixture::fixture();
    let original = b[2 * M..2 * M + 4096].to_vec();
    let e = entry(
        if data { Some(&original) } else { None },
        1,
        0,
        2 * M as u64,
        6 * M as u64,
    );
    b[M..M + e.len()].copy_from_slice(&e);
    for o in [65536, 131072] {
        b[o + 48..o + 64].fill(0x77);
        crc(&mut b[o..o + 4096]);
    }
    b[2 * M..2 * M + 4096].fill(0xff);
    b
}
#[test]
fn portable_vhdx_active_redo_overlay_rejects_corruption() {
    let bytes = dirty(true);
    assert!(Vhdx::open(memory(bytes.clone(), 20)).is_err());
    let image = Vhdx::open_recovered(memory(bytes.clone(), 21)).unwrap();
    let mut out = [0; 4];
    image.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [17; 4]);
    let mut corrupt = bytes;
    corrupt[M + 4105] ^= 1;
    assert!(Vhdx::open_recovered(memory(corrupt, 22)).is_err());
}
#[test]
fn portable_parented_flat_split_uses_supplied_authority_and_rejects_ancestor_alias() {
    let parent = Arc::new(
        Vmdk::open_descriptor_bound(
            descriptor(),
            &[
                VmdkExtentBinding {
                    name: "a".into(),
                    source: memory(vec![42; 512], 2),
                },
                VmdkExtentBinding {
                    name: "b".into(),
                    source: memory(vec![17; 1024], 3),
                },
            ],
            None,
            ParserLimits::default(),
        )
        .unwrap(),
    );
    let child_descriptor = br#"version=1
CID=87654321
parentCID=12345678
parentFileNameHint="untrusted"
createType="twoGbMaxExtentFlat"
RW 1 FLAT "c" 0
RW 1 FLAT "d" 0
"#;
    let bindings = [
        VmdkExtentBinding {
            name: "c".into(),
            source: memory(vec![9; 512], 5),
        },
        VmdkExtentBinding {
            name: "d".into(),
            source: memory(vec![8; 512], 6),
        },
    ];
    let image = Vmdk::open_descriptor_bound(
        memory(child_descriptor.to_vec(), 4),
        &bindings,
        Some(parent.clone()),
        ParserLimits::default(),
    )
    .unwrap();
    let mut out = [0; 4];
    image.read_exact_at(510, &mut out).unwrap();
    assert_eq!(out, [9, 9, 8, 8]);
    assert!(
        Vmdk::open_descriptor_bound(
            Arc::new(Memory {
                bytes: child_descriptor.to_vec(),
                identity: parent.source_identity()
            }),
            &bindings,
            Some(parent.clone()),
            ParserLimits::default()
        )
        .is_err()
    );
    let aliases = [
        VmdkExtentBinding {
            name: "c".into(),
            source: memory(vec![9; 512], 1),
        },
        VmdkExtentBinding {
            name: "d".into(),
            source: memory(vec![8; 512], 6),
        },
    ];
    assert!(
        Vmdk::open_descriptor_bound(
            memory(child_descriptor.to_vec(), 4),
            &aliases,
            Some(parent),
            ParserLimits::default()
        )
        .is_err()
    );
}
#[test]
fn portable_sparse_split_child_reads_inherited_grains_and_shared_budget() {
    let parent_descriptor = b"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 2 SPARSE \"a\"\nRW 2 SPARSE \"b\"\n";
    let parent = Arc::new(
        Vmdk::open_descriptor_bound(
            memory(parent_descriptor.to_vec(), 30),
            &[
                VmdkExtentBinding {
                    name: "a".into(),
                    source: memory(sparse(), 31),
                },
                VmdkExtentBinding {
                    name: "b".into(),
                    source: memory(sparse(), 32),
                },
            ],
            None,
            ParserLimits::default(),
        )
        .unwrap(),
    );
    let child_descriptor=b"version=1\nCID=87654321\nparentCID=12345678\nparentFileNameHint=\"untrusted\"\ncreateType=\"twoGbMaxExtentSparse\"\nRW 2 SPARSE \"c\"\nRW 2 SPARSE \"d\"\n";
    let mut empty = sparse();
    empty[1024..1028].fill(0);
    let child = Vmdk::open_descriptor_bound(
        memory(child_descriptor.to_vec(), 33),
        &[
            VmdkExtentBinding {
                name: "c".into(),
                source: memory(empty.clone(), 34),
            },
            VmdkExtentBinding {
                name: "d".into(),
                source: memory(empty, 35),
            },
        ],
        Some(parent.clone()),
        ParserLimits::default(),
    )
    .unwrap();
    let before = parent.budget().unwrap().usage();
    let mut out = [0; 4];
    child.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [42; 4]);
    let after = parent.budget().unwrap().usage();
    assert!(after.work_items > before.work_items);
}

#[test]
fn portable_descriptor_parent_rejects_changed_and_invalid_limits_before_charging() {
    let parent = Arc::new(
        Vmdk::open_descriptor_bound(
            descriptor(),
            &[
                VmdkExtentBinding {
                    name: "a".into(),
                    source: memory(vec![42; 512], 2),
                },
                VmdkExtentBinding {
                    name: "b".into(),
                    source: memory(vec![17; 1024], 3),
                },
            ],
            None,
            ParserLimits::default(),
        )
        .unwrap(),
    );
    let before = parent.budget().unwrap().usage();
    let tighter = ParserLimits {
        metadata_bytes: 4,
        ..Default::default()
    };
    let error = Vmdk::open_descriptor_bound(descriptor(), &[], Some(parent.clone()), tighter)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let invalid = ParserLimits {
        work_items: u64::MAX,
        ..Default::default()
    };
    assert!(Vmdk::open_descriptor_bound(descriptor(), &[], Some(parent.clone()), invalid).is_err());
    assert_eq!(parent.budget().unwrap().usage(), before);
}
#[cfg(feature = "std")]
#[test]
fn portable_bound_child_rejects_extent_alias_of_host_parent() {
    let directory = tempfile::tempdir().unwrap();
    let descriptor_path = directory.path().join("parent.vmdk");
    let a = directory.path().join("a");
    let b = directory.path().join("b");
    std::fs::write(&a, vec![42; 512]).unwrap();
    std::fs::write(&b, vec![17; 1024]).unwrap();
    std::fs::write(&descriptor_path, b"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\nRW 1 FLAT \"a\" 0\nRW 1 FLAT \"b\" 1\n").unwrap();
    let parent = Arc::new(Vmdk::open_chain(&descriptor_path, &[a.clone(), b]).unwrap());
    let child = memory(b"version=1\nCID=87654321\nparentCID=12345678\nparentFileNameHint=\"untrusted\"\ncreateType=\"twoGbMaxExtentFlat\"\nRW 1 FLAT \"c\" 0\nRW 1 FLAT \"d\" 0\n".to_vec(), 50);
    let bindings = [
        VmdkExtentBinding {
            name: "c".into(),
            source: Arc::new(virtdisk::RawDisk::open(a).unwrap()),
        },
        VmdkExtentBinding {
            name: "d".into(),
            source: memory(vec![0; 512], 51),
        },
    ];
    let error =
        Vmdk::open_descriptor_bound(child, &bindings, Some(parent), ParserLimits::default())
            .err()
            .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("aliases"));
}
