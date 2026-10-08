use std::sync::Arc;
use virtdisk::io;
use virtdisk::{ParserLimits, ReadAt, Vmdk};
#[path = "support/bytes.rs"]
mod bytes;
use bytes::Bytes;

fn fixture() -> Vec<u8> {
    let mut b = vec![0; 2048];
    b[..4].copy_from_slice(b"KDMV");
    b[4..8].copy_from_slice(&1u32.to_le_bytes());
    b[12..20].copy_from_slice(&2u64.to_le_bytes());
    b[20..28].copy_from_slice(&1u64.to_le_bytes());
    b[44..48].copy_from_slice(&2u32.to_le_bytes());
    b[56..64].copy_from_slice(&1u64.to_le_bytes());
    b[64..72].copy_from_slice(&3u64.to_le_bytes());
    b[512..516].copy_from_slice(&2u32.to_le_bytes());
    b[1024..1028].copy_from_slice(&3u32.to_le_bytes());
    b[1536..].fill(42);
    b
}
fn open(b: Vec<u8>) -> io::Result<Vmdk> {
    Vmdk::open(Arc::new(Bytes(b)))
}
#[test]
fn reads_allocated_and_sparse_grains_with_bounds() {
    let d = open(fixture()).unwrap();
    assert_eq!(d.len(), 1024);
    let mut out = [1; 12];
    d.read_exact_at(506, &mut out).unwrap();
    assert_eq!(out, [42, 42, 42, 42, 42, 42, 0, 0, 0, 0, 0, 0]);
    assert!(d.read_exact_at(1024, &mut []).is_ok());
    assert!(d.read_exact_at(1025, &mut []).is_err());
    assert!(d.read_exact_at(u64::MAX, &mut out).is_err());
}
#[test]
fn rejects_corruption_and_unsupported_profiles() {
    for (offset, bytes) in [
        (0, b"BAD!".to_vec()),
        (20, 0u64.to_le_bytes().to_vec()),
        (512, 99u32.to_le_bytes().to_vec()),
        (1024, 2u32.to_le_bytes().to_vec()),
        (8, (1u32 << 16).to_le_bytes().to_vec()),
        (72, vec![1]),
    ] {
        let mut b = fixture();
        b[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(open(b).is_err(), "offset {offset}");
    }
}
#[test]
fn rejects_aliases_and_tight_budgets() {
    let mut b = fixture();
    b[1028..1032].copy_from_slice(&3u32.to_le_bytes());
    assert!(open(b).is_err());
    let limits = ParserLimits {
        metadata_bytes: 4,
        ..Default::default()
    };
    assert!(Vmdk::open_with_limits(Arc::new(Bytes(fixture())), limits).is_err());
}

#[test]
fn explicit_zero_grain_requires_feature_and_descriptor_parent_is_rejected() {
    let mut b = fixture();
    b[8..12].copy_from_slice(&4u32.to_le_bytes());
    b[1028..1032].copy_from_slice(&1u32.to_le_bytes());
    let d = open(b).unwrap();
    let mut zero = [9; 512];
    d.read_exact_at(512, &mut zero).unwrap();
    assert_eq!(zero, [0; 512]);
    let mut b = fixture();
    b.resize(2560, 0);
    b[28..36].copy_from_slice(&4u64.to_le_bytes());
    b[36..44].copy_from_slice(&1u64.to_le_bytes());
    b[64..72].copy_from_slice(&5u64.to_le_bytes());
    let desc = b"parentCID=12345678\nparentFileNameHint=\"secret\"\n";
    b[2048..2048 + desc.len()].copy_from_slice(desc);
    assert_eq!(open(b).err().unwrap().kind(), io::ErrorKind::Unsupported);
}

#[test]
fn rejects_mapping_metadata_overlap_and_geometry_overflow() {
    let mut b = fixture();
    b[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(open(b).is_err());
    let mut b = fixture();
    b[56..64].copy_from_slice(&2u64.to_le_bytes());
    assert!(open(b).is_err());
    let mut b = fixture();
    b[44..48].copy_from_slice(&(1u32 << 31).to_le_bytes());
    assert!(open(b).is_err());
}

#[cfg(feature = "std")]
#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_hosted_sparse_matches_raw() {
    use std::{fs, process::Command};
    use virtdisk::RawDisk;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("source.raw");
    let image = dir.path().join("disk.vmdk");
    let mut bytes = vec![0; 3 * 65536 + 512];
    for (i, b) in bytes[..65536].iter_mut().enumerate() {
        *b = i as u8;
    }
    bytes[131072..].fill(0xa5);
    fs::write(&raw, &bytes).unwrap();
    assert!(
        Command::new("qemu-img")
            .args(["convert", "-f", "raw", "-O", "vmdk"])
            .arg(&raw)
            .arg(&image)
            .status()
            .unwrap()
            .success()
    );
    let disk = Vmdk::open(Arc::new(RawDisk::open(&image).unwrap())).unwrap();
    let mut actual = vec![0; bytes.len()];
    disk.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, bytes);
}

#[test]
fn redundant_directory_must_match_and_own_separate_tables() {
    let mut b = fixture();
    b.resize(3072, 0);
    b[1536..].fill(0);
    b[8..12].copy_from_slice(&2u32.to_le_bytes());
    b[48..56].copy_from_slice(&3u64.to_le_bytes());
    b[64..72].copy_from_slice(&5u64.to_le_bytes());
    b[1024..1028].copy_from_slice(&5u32.to_le_bytes());
    b[1536..1540].copy_from_slice(&4u32.to_le_bytes());
    b[2048..2052].copy_from_slice(&5u32.to_le_bytes());
    b[2560..].fill(42);
    open(b.clone()).unwrap();
    b[2048..2052].fill(0);
    assert!(open(b.clone()).is_err());
    b[2048..2052].copy_from_slice(&5u32.to_le_bytes());
    b[1536..1540].copy_from_slice(&2u32.to_le_bytes());
    assert!(open(b).is_err());
}

#[cfg(feature = "std")]
#[test]
fn descriptor_flat_split_requires_explicit_extent_authorization() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    let descriptor = dir.path().join("disk.vmdk");
    let first = dir.path().join("first extent.bin");
    let second = dir.path().join("second.bin");
    fs::write(&first, vec![3; 1024]).unwrap();
    fs::write(&second, vec![8; 512]).unwrap();
    fs::write(&descriptor,"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\nRW 1 FLAT \"first extent.bin\" 1\nRW 1 FLAT \"second.bin\" 0\n").unwrap();
    assert!(Vmdk::open_descriptor(&descriptor, &[]).is_err());
    let d = Vmdk::open_descriptor(&descriptor, &[first.clone(), second.clone()]).unwrap();
    let mut data = [0; 8];
    d.read_exact_at(508, &mut data).unwrap();
    assert_eq!(data, [3, 3, 3, 3, 8, 8, 8, 8]);
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
            (0, 512, virtdisk::ExtentKind::Allocated),
            (512, 512, virtdisk::ExtentKind::Allocated)
        ]
    );

    assert_eq!(d.len(), 1024);
    fs::write(
        &descriptor,
        "version=1\nCID=12345678\nparentCID=12345678\nRW 1 FLAT \"second.bin\" 0\n",
    )
    .unwrap();
    assert!(Vmdk::open_descriptor(&descriptor, &[first, second]).is_err());
}

#[cfg(feature = "std")]
#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_flat_descriptor_matches_raw() {
    use std::{fs, process::Command};
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw");
    let image = dir.path().join("disk.vmdk");
    let bytes = vec![19; 196608];
    fs::write(&raw, &bytes).unwrap();
    assert!(
        Command::new("qemu-img")
            .args([
                "convert",
                "-f",
                "raw",
                "-O",
                "vmdk",
                "-o",
                "subformat=monolithicFlat"
            ])
            .arg(&raw)
            .arg(&image)
            .status()
            .unwrap()
            .success()
    );
    let disk = Vmdk::open_descriptor(&image, &[dir.path().join("disk-flat.vmdk")]).unwrap();
    let mut actual = vec![0; bytes.len()];
    disk.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, bytes);
}

#[cfg(feature = "std")]
#[test]
fn descriptor_sparse_extent_bounds_and_repeated_paths_fail_closed() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    let descriptor = dir.path().join("disk.vmdk");
    let sparse = dir.path().join("extent.vmdk");
    fs::write(&sparse, fixture()).unwrap();
    let prefix =
        "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\n";
    fs::write(
        &descriptor,
        format!("{prefix}RW 2 SPARSE \"extent.vmdk\"\n"),
    )
    .unwrap();
    let d = Vmdk::open_descriptor(&descriptor, std::slice::from_ref(&sparse)).unwrap();
    let mut data = [0; 4];
    d.read_exact_at(0, &mut data).unwrap();
    assert_eq!(data, [42; 4]);
    for extent in [
        "RW 3 SPARSE \"extent.vmdk\"\n",
        "RW 2 SPARSE \"extent.vmdk\"\nRW 2 SPARSE \"extent.vmdk\"\n",
        "RW 1 FLAT \"extent.vmdk\" 18446744073709551615\n",
        "RW 1 VMFS \"extent.vmdk\"\n",
    ] {
        fs::write(&descriptor, format!("{prefix}{extent}")).unwrap();
        assert!(Vmdk::open_descriptor(&descriptor, std::slice::from_ref(&sparse)).is_err());
    }
    let limits = ParserLimits {
        attribute_bytes: 4,
        ..Default::default()
    };
    assert!(Vmdk::open_descriptor_with_limits(&descriptor, &[sparse], limits).is_err());
}

#[test]
fn warm_sparse_reads_charge_work_and_transient_metadata_obeys_cache() {
    let mut b = fixture();
    b[1024..1028].fill(0);
    let limits = ParserLimits {
        work_items: 16,
        ..Default::default()
    };
    let disk = Vmdk::open_with_limits(Arc::new(Bytes(b)), limits).unwrap();
    let mut out = [0; 1];
    let mut failed = false;
    for _ in 0..32 {
        if disk.read_exact_at(0, &mut out).is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed);
    let limits = ParserLimits {
        cache_bytes: 256,
        ..Default::default()
    };
    assert!(Vmdk::open_with_limits(Arc::new(Bytes(fixture())), limits).is_err());
}

#[cfg(feature = "std")]
#[test]
fn descriptor_rejects_hardlink_aliases_and_missing_or_duplicate_create_type() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    let descriptor = dir.path().join("disk.vmdk");
    let extent = dir.path().join("one.bin");
    let alias = dir.path().join("alias.bin");
    fs::write(&extent, vec![1; 512]).unwrap();
    fs::hard_link(&extent, &alias).unwrap();
    let prefix = "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\n";
    fs::write(
        &descriptor,
        format!("{prefix}RW 1 FLAT \"one.bin\" 0\nRW 1 FLAT \"alias.bin\" 0\n"),
    )
    .unwrap();
    assert!(Vmdk::open_descriptor(&descriptor, &[extent.clone(), alias]).is_err());
    let self_alias = dir.path().join("self.bin");
    fs::hard_link(&descriptor, &self_alias).unwrap();
    fs::write(&descriptor, format!("{prefix}RW 1 FLAT \"self.bin\" 0\n")).unwrap();
    assert!(Vmdk::open_descriptor(&descriptor, &[self_alias]).is_err());
    for config in [
        "version=1\nCID=12345678\nparentCID=ffffffff\nRW 1 FLAT \"one.bin\" 0\n".to_owned(),
        format!("{prefix}createType=\"twoGbMaxExtentFlat\"\nRW 1 FLAT \"one.bin\" 0\n"),
    ] {
        fs::write(&descriptor, config).unwrap();
        assert!(Vmdk::open_descriptor(&descriptor, std::slice::from_ref(&extent)).is_err());
    }
}

#[test]
fn extent_visitor_reports_grains_without_payload_and_cancels() {
    use virtdisk::ExtentKind;
    let d = open(fixture()).unwrap();
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
            (512, 512, ExtentKind::Zero)
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
fn map_visitation_never_reads_payload_and_exhausts_work() {
    struct MetadataOnly(Bytes);
    impl ReadAt for MetadataOnly {
        fn len(&self) -> u64 {
            self.0.len()
        }
        fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
            if offset >= 1536 {
                return Err(io::Error::other("payload read forbidden"));
            }
            self.0.read_exact_at(offset, out)
        }
    }
    let d = Vmdk::open_with_limits(
        Arc::new(MetadataOnly(Bytes(fixture()))),
        ParserLimits {
            work_items: 24,
            ..Default::default()
        },
    )
    .unwrap();
    let mut failed = false;
    for _ in 0..32 {
        if d.visit_extents(&mut |_| Ok(())).is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed);
}

#[cfg(feature = "std")]
#[test]
fn authorized_parent_chain_inherits_but_explicit_zero_masks() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vmdk");
    let child = dir.path().join("child.vmdk");
    fn described(mut b: Vec<u8>, cid: &str, parent: &str, hint: &str) -> Vec<u8> {
        b.resize(3072, 0);
        b[28..36].copy_from_slice(&4u64.to_le_bytes());
        b[36..44].copy_from_slice(&2u64.to_le_bytes());
        b[64..72].copy_from_slice(&6u64.to_le_bytes());
        // Payload must live after all metadata.
        b[1024..1028].copy_from_slice(&6u32.to_le_bytes());
        b.resize(3584, 42);
        let desc = format!(
            "version=1\nCID={cid}\nparentCID={parent}\ncreateType=\"monolithicSparse\"\n{hint}\nRW 2 SPARSE \"self.vmdk\"\n"
        );
        b[2048..2048 + desc.len()].copy_from_slice(desc.as_bytes());
        b
    }
    std::fs::write(&base, described(fixture(), "12345678", "ffffffff", "")).unwrap();
    let mut bytes = described(
        fixture(),
        "87654321",
        "12345678",
        "parentFileNameHint=\"base.vmdk\"",
    );
    bytes[8..12].copy_from_slice(&4u32.to_le_bytes());
    bytes[1024..1028].fill(0);
    bytes[1028..1032].copy_from_slice(&1u32.to_le_bytes());
    std::fs::write(&child, &bytes).unwrap();
    assert!(Vmdk::open_chain(&child, &[]).is_err());
    let disk = Vmdk::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    let mut out = [9; 1024];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(&out[..512], &[42; 512]);
    assert_eq!(&out[512..], &[0; 512]);
    let mut extents = Vec::new();
    disk.visit_extents(&mut |e| {
        extents.push(e.kind);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        extents,
        [virtdisk::ExtentKind::Inherited, virtdisk::ExtentKind::Zero]
    );
    let limits = ParserLimits {
        recursion_depth: 1,
        ..Default::default()
    };
    assert!(Vmdk::open_chain_with_limits(&child, std::slice::from_ref(&base), limits).is_err());
    let bad = String::from_utf8_lossy(&bytes[2048..3072])
        .replace("parentCID=12345678", "parentCID=12345679");
    bytes[2048..3072].copy_from_slice(bad.as_bytes());
    std::fs::write(&child, bytes).unwrap();
    assert!(Vmdk::open_chain(&child, &[base]).is_err());
}

#[cfg(feature = "std")]
#[test]
fn external_split_sparse_chain_and_flat_parent_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vmdk");
    let flat = dir.path().join("base-flat.vmdk");
    let child = dir.path().join("child.vmdk");
    let first = dir.path().join("first.vmdk");
    let second = dir.path().join("second.vmdk");
    std::fs::write(&flat, [7; 2048]).unwrap();
    std::fs::write(&base, "version=1\nCID=1\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\nRW 4 FLAT \"base-flat.vmdk\" 0\n").unwrap();
    let mut hole = fixture();
    hole[1024..1032].fill(0);
    std::fs::write(&first, &hole).unwrap();
    std::fs::write(&second, &hole).unwrap();
    let text = "version=1\nCID=2\nparentCID=1\nparentFileNameHint=\"base.vmdk\"\ncreateType=\"twoGbMaxExtentSparse\"\nRW 2 SPARSE \"first.vmdk\"\nRW 2 SPARSE \"second.vmdk\"\n";
    std::fs::write(&child, text).unwrap();
    let authorized = [base.clone(), flat.clone(), first.clone(), second.clone()];
    let disk = Vmdk::open_chain(&child, &authorized).unwrap();
    let mut out = [0; 24];
    disk.read_exact_at(1016, &mut out).unwrap();
    assert_eq!(out, [7; 24]);
    let mut kinds = Vec::new();
    disk.visit_extents(&mut |e| {
        kinds.push(e.kind);
        Ok(())
    })
    .unwrap();
    assert_eq!(kinds, [virtdisk::ExtentKind::Inherited; 4]);
    // Every authorized sparse extent still rejects pending metadata transactions.
    let pending = first.with_file_name("first.vmdk.virtdisk-transaction");
    std::fs::write(&pending, b"pending").unwrap();
    assert!(Vmdk::open_chain(&child, &authorized).is_err());
    std::fs::remove_file(&pending).unwrap();
    // CID changes invalidate an already declared linkage on the next open.
    std::fs::write(&base, "version=1\nCID=3\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\nRW 4 FLAT \"base-flat.vmdk\" 0\n").unwrap();
    assert!(Vmdk::open_chain(&child, &authorized).is_err());
    // A descriptor hard-link cycle must not rely on canonical spelling alone.
    let alias = dir.path().join("alias.vmdk");
    std::fs::hard_link(&child, &alias).unwrap();
    std::fs::write(&child, text.replace("base.vmdk", "alias.vmdk")).unwrap();
    assert!(Vmdk::open_chain(&child, &[alias, first, second]).is_err());
}

#[cfg(feature = "std")]
#[test]
#[ignore = "requires qemu-img independent VMDK backing oracle"]
fn qemu_created_hosted_parent_chain_matches_backing() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("source.raw");
    let base = dir.path().join("base.vmdk");
    let child = dir.path().join("child.vmdk");
    let bytes = vec![83; 131072];
    std::fs::write(&raw, &bytes).unwrap();
    assert!(
        Command::new("qemu-img")
            .args(["convert", "-f", "raw", "-O", "vmdk"])
            .arg(&raw)
            .arg(&base)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("qemu-img")
            .args(["create", "-f", "vmdk", "-F", "vmdk", "-b"])
            .arg(&base)
            .arg(&child)
            .status()
            .unwrap()
            .success()
    );
    let disk = Vmdk::open_chain(&child, &[base]).unwrap();
    let mut out = vec![0; bytes.len()];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, bytes);
    let info = Command::new("qemu-img")
        .args(["info", "--backing-chain", "--output=json"])
        .arg(&child)
        .output()
        .unwrap();
    assert!(info.status.success());
    assert!(
        String::from_utf8(info.stdout)
            .unwrap()
            .contains("backing-filename")
    );
}

#[cfg(feature = "std")]
#[test]
#[ignore = "requires qemu-img independent flat/split backing oracle"]
fn qemu_external_flat_parent_and_split_sparse_child() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("source.raw");
    let base = dir.path().join("base.vmdk");
    let child = dir.path().join("child.vmdk");
    std::fs::write(&raw, vec![69; 131072]).unwrap();
    assert!(
        Command::new("qemu-img")
            .args([
                "convert",
                "-f",
                "raw",
                "-O",
                "vmdk",
                "-o",
                "subformat=monolithicFlat"
            ])
            .arg(&raw)
            .arg(&base)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "vmdk",
                "-F",
                "vmdk",
                "-o",
                "subformat=twoGbMaxExtentSparse",
                "-b"
            ])
            .arg(&base)
            .arg(&child)
            .status()
            .unwrap()
            .success()
    );
    let allowed = [
        base,
        dir.path().join("base-flat.vmdk"),
        dir.path().join("child-s001.vmdk"),
    ];
    let disk = Vmdk::open_chain(&child, &allowed).unwrap();
    let mut out = vec![0; 131072];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, vec![69; 131072]);
}

#[cfg(feature = "std")]
#[test]
fn chain_descriptor_linkage_profiles_and_cumulative_budgets_are_strict() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.vmdk");
    let child = dir.path().join("child.vmdk");
    drop(virtdisk::VmdkWriter::create(&base, 65536).unwrap());
    drop(virtdisk::VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base)).unwrap());
    let original = std::fs::read(&child).unwrap();
    let end = original[512..10752].iter().position(|b| *b == 0).unwrap() + 512;
    let descriptor = std::str::from_utf8(&original[512..end]).unwrap();
    for altered in [
        format!("{descriptor}CID=00000001\n"),
        format!("{descriptor}parentCID=00000001\n"),
        format!("{descriptor}createType=\"monolithicSparse\"\n"),
        descriptor.replace("RW 128 SPARSE", "RW 129 SPARSE"),
        descriptor.replace("RW 128 SPARSE", "RW 128 FLAT"),
        descriptor.replace("monolithicSparse", "streamOptimized"),
        descriptor
            .lines()
            .map(|line| {
                if line.starts_with("parentFileNameHint=") {
                    "parentFileNameHint=\"file://unauthorized\""
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    ] {
        let mut bytes = original.clone();
        bytes[512..10752].fill(0);
        bytes[512..512 + altered.len()].copy_from_slice(altered.as_bytes());
        std::fs::write(&child, bytes).unwrap();
        assert!(Vmdk::open_chain(&child, std::slice::from_ref(&base)).is_err());
    }
    std::fs::write(&child, original).unwrap();
    for limits in [
        ParserLimits {
            metadata_bytes: 15000,
            ..Default::default()
        },
        ParserLimits {
            cache_bytes: 20000,
            ..Default::default()
        },
    ] {
        assert!(Vmdk::open_chain_with_limits(&child, std::slice::from_ref(&base), limits).is_err());
    }
}

#[test]
fn hosted_v2_zero_feature_preserves_allocation_and_masking() {
    for flags in 0..=7u32 {
        let mut bytes = fixture();
        bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        if flags & 1 != 0 {
            bytes[73..77].copy_from_slice(&[10, 32, 13, 10]);
        }
        if flags & 4 != 0 {
            bytes[1028..1032].copy_from_slice(&1u32.to_le_bytes());
        }
        if flags & 2 != 0 {
            bytes.resize(4096, 0);
            bytes[48..56].copy_from_slice(&4u64.to_le_bytes());
            bytes[64..72].copy_from_slice(&7u64.to_le_bytes());
            bytes[1024..1028].copy_from_slice(&7u32.to_le_bytes());
            bytes[2048..2052].copy_from_slice(&5u32.to_le_bytes());
            let table = bytes[1024..1032].to_vec();
            bytes[2560..2568].copy_from_slice(&table);
            bytes[3584..4096].fill(42);
        }
        let disk = open(bytes).unwrap();
        let mut actual = [9; 1024];
        disk.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(&actual[..512], &[42; 512], "flags={flags}");
        assert_eq!(&actual[512..], &[0; 512], "flags={flags}");
    }
    let mut zero = fixture();
    zero[4..8].copy_from_slice(&2u32.to_le_bytes());
    zero[1028..1032].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(open(zero).err().unwrap().kind(), io::ErrorKind::InvalidData);
    for (offset, value) in [
        (4, 3u32.to_le_bytes().to_vec()),
        (4, 0u32.to_le_bytes().to_vec()),
        (8, 12u32.to_le_bytes().to_vec()),
        (72, vec![1]),
        (77, vec![1]),
        (8, 5u32.to_le_bytes().to_vec()),
    ] {
        let mut bad = fixture();
        bad[4..8].copy_from_slice(&2u32.to_le_bytes());
        bad[offset..offset + value.len()].copy_from_slice(&value);
        let expected = if offset == 8 && value == 5u32.to_le_bytes() {
            io::ErrorKind::InvalidData
        } else {
            io::ErrorKind::Unsupported
        };
        assert_eq!(open(bad).err().unwrap().kind(), expected, "offset={offset}");
    }
}
