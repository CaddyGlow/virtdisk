use std::{io, sync::Arc};
use virtdisk::{ParserLimits, ReadAt, Vhdx};
#[path = "support/bytes.rs"]
mod bytes;
#[path = "support/vhdx.rs"]
mod fixture_support;
use bytes::Bytes;
use fixture_support::{M, fixture, put, put64};
fn open(b: Vec<u8>) -> io::Result<Vhdx> {
    Vhdx::open(Arc::new(Bytes(b)))
}
#[path = "support/vhdx_chain.rs"]
mod chain_support;
use chain_support::child;
#[test]
fn native_partial_sectors_inherit_and_zero_masks() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    std::fs::write(&parent, fixture()).unwrap();
    std::fs::write(&path, child()).unwrap();
    let image = Vhdx::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
    let mut out = [0; 4];
    image.read_exact_at(510, &mut out).unwrap();
    assert_eq!(out, [99, 99, 17, 17]);
    image.read_exact_at(M as u64, &mut out).unwrap();
    assert_eq!(out, [23; 4]);
    let mut b = child();
    put64(&mut b, 2 * M + 8, 2);
    std::fs::write(&path, b).unwrap();
    let image = Vhdx::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
    image.read_exact_at(M as u64, &mut out).unwrap();
    assert_eq!(out, [0; 4]);
    assert!(open(child()).is_err());
}
#[test]
fn explicit_authorization_linkage_and_bitmap_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    std::fs::write(&parent, fixture()).unwrap();
    std::fs::write(&path, child()).unwrap();
    assert_eq!(
        Vhdx::open_chain(&path, &[]).err().unwrap().kind(),
        io::ErrorKind::PermissionDenied
    );
    for mutation in 0..4 {
        let mut b = child();
        match mutation {
            0 => put64(&mut b, 2 * M + 4096 * 8, 0),
            1 => put64(&mut b, 2 * M + 4096 * 8, (4 * M as u64) | 6),
            2 => put64(&mut b, 2 * M + 16, 6),
            _ => b[3 * M + 65616 + 44 + "parent_linkage".len() * 2 + 2] = b'a',
        };
        std::fs::write(&path, b).unwrap();
        assert!(
            Vhdx::open_chain(&path, std::slice::from_ref(&parent)).is_err(),
            "mutation {mutation}"
        );
    }
    std::fs::write(&path, child()).unwrap();
    assert!(
        Vhdx::open_chain_with_limits(
            &path,
            &[parent],
            ParserLimits {
                recursion_depth: 1,
                ..Default::default()
            }
        )
        .is_err()
    );
}
#[test]
fn hard_link_cycle_and_readonly_sources() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    std::fs::write(&path, child()).unwrap();
    std::fs::hard_link(&path, &parent).unwrap();
    assert!(Vhdx::open_chain(&path, &[parent]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), child());
}
#[test]
fn creates_native_child_with_inheritance_and_preserved_disk_identity() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("new.vhdx");
    std::fs::write(&parent, fixture()).unwrap();
    virtdisk::create_vhdx_overlay(&path, &parent, &[]).unwrap();
    let image = Vhdx::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
    let mut out = [0; 4];
    image.read_exact_at(M as u64 - 2, &mut out).unwrap();
    assert_eq!(out, [17, 17, 23, 23]);
    assert!(virtdisk::create_vhdx_overlay(&path, &parent, &[]).is_err());
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        &bytes[3 * M + 65568..3 * M + 65584],
        &fixture()[3 * M + 65568..3 * M + 65584]
    );
    assert_ne!(&bytes[131072 + 32..131072 + 48], &[0; 16]);
}
#[test]
fn maps_partial_inherited_private_and_zero_sectors() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    std::fs::write(&parent, fixture()).unwrap();
    std::fs::write(&path, child()).unwrap();
    let image = Vhdx::open_chain(&path, &[parent]).unwrap();
    let mut extents = Vec::new();
    image
        .visit_extents(&mut |e| {
            extents.push(e);
            Ok(())
        })
        .unwrap();
    assert_eq!(extents.len(), 2);
    assert_eq!(extents[0].length, 512);
    assert_eq!(extents[0].kind, virtdisk::ExtentKind::Allocated);
    assert_eq!(extents[1].kind, virtdisk::ExtentKind::Inherited);
}
#[test]
fn native_writer_promotes_inherited_and_partial_blocks_without_parent_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    let original = fixture();
    std::fs::write(&parent, &original).unwrap();
    std::fs::write(&path, child()).unwrap();
    let writer = virtdisk::VhdxWriter::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(513, &[42]).unwrap();
    writer.write_zeroes(M as u64 + 3, 1).unwrap();
    writer.flush().unwrap();
    let mut out = [0; 4];
    writer.read_exact_at(510, &mut out).unwrap();
    assert_eq!(out, [99, 99, 17, 42]);
    writer.read_exact_at(M as u64 + 1, &mut out).unwrap();
    assert_eq!(out, [23, 23, 0, 23]);
    drop(writer);
    let image = Vhdx::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
    image.read_exact_at(510, &mut out).unwrap();
    assert_eq!(out, [99, 99, 17, 42]);
    assert_eq!(std::fs::read(&parent).unwrap(), original);
}
#[test]
fn writer_creates_locked_native_child_and_fences_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    std::fs::write(&parent, fixture()).unwrap();
    let writer = virtdisk::VhdxWriter::create_overlay(&path, &parent, &[]).unwrap();
    assert!(virtdisk::VhdxWriter::open_chain(&path, std::slice::from_ref(&parent)).is_err());
    writer.write_all_at(0, &[8]).unwrap();
    let mut out = [0; 2];
    writer.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [8, 17]);
    drop(writer);
    assert!(Vhdx::open_chain(&path, &[parent]).is_ok());
}
#[test]
fn native_4096_sector_bitmap_and_shared_limits() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    let mut p = fixture();
    put(&mut p, 3 * M + 65584, 4096);
    std::fs::write(&parent, p).unwrap();
    let mut b = child();
    put(&mut b, 3 * M + 65584, 4096);
    put64(&mut b, 2 * M + 4096 * 8, 0);
    put64(&mut b, 2 * M + 32768 * 8, (6 * M as u64) | 6);
    std::fs::write(&path, b).unwrap();
    let image = Vhdx::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
    let mut out = [0; 4];
    image.read_exact_at(4094, &mut out).unwrap();
    assert_eq!(out, [99, 99, 17, 17]);
    assert!(
        Vhdx::open_chain_with_limits(
            &path,
            std::slice::from_ref(&parent),
            ParserLimits {
                work_items: 10,
                ..Default::default()
            }
        )
        .is_err()
    );
    let grandchild = dir.path().join("grandchild.vhdx");
    assert!(virtdisk::create_vhdx_overlay(&grandchild, &path, &[]).is_err());
    assert!(!grandchild.exists());
    virtdisk::create_vhdx_overlay(&grandchild, &path, std::slice::from_ref(&parent)).unwrap();
    let image = Vhdx::open_chain(&grandchild, &[path, parent]).unwrap();
    image.read_exact_at(4094, &mut out).unwrap();
    assert_eq!(out, [99, 99, 17, 17]);
}
#[test]
fn optional_virtual_metadata_survives_native_fork() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    let mut p = fixture();
    p[3 * M + 10] = 6;
    let e = 3 * M + 32 + 5 * 32;
    let id = [71; 16];
    p[e..e + 16].copy_from_slice(&id);
    put(&mut p, e + 16, 65616);
    let optional: Vec<u8> = (0..131073).map(|i| (i % 251) as u8).collect();
    put(&mut p, e + 20, optional.len() as u32);
    put(&mut p, e + 24, 2);
    p[3 * M + 65616..3 * M + 65616 + optional.len()].copy_from_slice(&optional);
    std::fs::write(&parent, &p).unwrap();
    virtdisk::create_vhdx_overlay(&path, &parent, &[]).unwrap();
    let b = std::fs::read(&path).unwrap();
    let e = (0..7)
        .map(|i| 3 * M + 32 + i * 32)
        .find(|&e| b[e..e + 16] == id)
        .unwrap();
    let off = u32::from_le_bytes(b[e + 16..e + 20].try_into().unwrap()) as usize;
    assert_eq!(&b[3 * M + off..3 * M + off + optional.len()], optional);
    assert!(Vhdx::open_chain(&path, std::slice::from_ref(&parent)).is_ok());
    assert_eq!(std::fs::read(parent).unwrap(), p);
}
#[test]
#[ignore = "requires qemu-img; documents unavailable native differencing oracle separately from standalone flattening"]
fn qemu_native_differencing_gate_and_flattened_data_oracle() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let path = dir.path().join("child.vhdx");
    std::fs::write(&parent, fixture()).unwrap();
    virtdisk::create_vhdx_overlay(&path, &parent, &[]).unwrap();
    let output = std::process::Command::new("qemu-img")
        .args(["info", "-f", "vhdx"])
        .arg(&path)
        .output()
        .unwrap();
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        message.contains("differencing") || message.contains("Operation not supported"),
        "{message}"
    );
    eprintln!("Native VHDX differencing oracle unavailable: {message}");
    let image = Vhdx::open_chain(&path, &[parent]).unwrap();
    let flat = dir.path().join("flat.vhdx");
    virtdisk::create_vhdx(&flat, &image).unwrap();
    let checked = std::process::Command::new("qemu-img")
        .args(["check", "-f", "vhdx"])
        .arg(&flat)
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let raw = dir.path().join("flat.raw");
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vhdx", "-O", "raw"])
            .arg(flat)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    let b = std::fs::read(raw).unwrap();
    assert_eq!(&b[..M], &vec![17; M]);
    assert_eq!(&b[M..], &vec![23; M]);
}
