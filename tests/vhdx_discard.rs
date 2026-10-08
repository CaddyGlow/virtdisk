#![cfg(feature = "std")]
#![cfg(target_os = "linux")]
use virtdisk::{DiscardPolicy, DiscardResult, ReadAt, Vhdx, VhdxWriter};
const M: u64 = 1 << 20;
#[test]
fn whole_and_final_clipped_discard_release_mapping_and_mask_parent() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.vhdx");
    let child = dir.path().join("child.vhdx");
    let w = VhdxWriter::create(&parent, M + 512).unwrap();
    w.write_all_at(0, &vec![7; (M + 512) as usize]).unwrap();
    w.flush().unwrap();
    drop(w);
    let original = std::fs::read(&parent).unwrap();
    let w = VhdxWriter::create_overlay(&child, &parent, &[]).unwrap();
    w.write_all_at(100, &[9; 16]).unwrap();
    assert_eq!(
        w.discard(0, M, DiscardPolicy::RequireDeallocation).unwrap(),
        DiscardResult::Deallocated
    );
    assert_eq!(
        w.discard(M, 512, DiscardPolicy::RequireDeallocation)
            .unwrap(),
        DiscardResult::Deallocated
    );
    let mut bytes = vec![1; (M + 512) as usize];
    w.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes.iter().all(|b| *b == 0));
    w.write_all_at(3, &[9; 8]).unwrap();
    w.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes[3..11], &[9; 8]);
    assert!(bytes[..3].iter().chain(bytes[11..].iter()).all(|b| *b == 0));
    w.flush().unwrap();
    drop(w);
    let image = Vhdx::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    image.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes[..3].iter().chain(bytes[11..].iter()).all(|b| *b == 0));
    assert_eq!(std::fs::read(&parent).unwrap(), original);
}
#[test]
fn discard_checks_range_alignment_before_epoch_and_allows_explicit_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let w = VhdxWriter::create(&path, 2 * M).unwrap();
    w.write_all_at(0, &[7; 32]).unwrap();
    w.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(w.discard(1, M, DiscardPolicy::RequireDeallocation).is_err());
    assert!(
        w.discard(0, 512, DiscardPolicy::RequireDeallocation)
            .is_err()
    );
    assert!(
        w.discard(u64::MAX, M, DiscardPolicy::AllowZeroFallback)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        w.discard(1, 16, DiscardPolicy::AllowZeroFallback).unwrap(),
        DiscardResult::Zeroed
    );
    let mut bytes = [0; 32];
    w.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes[0], 7);
    assert_eq!(bytes[17], 7);
    assert_eq!(&bytes[1..17], &[0; 16]);
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_reads_standalone_zero_bat_discard() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vhdx");
    let raw = dir.path().join("oracle.raw");
    let w = VhdxWriter::create(&path, 2 * M).unwrap();
    w.write_all_at(0, &vec![7; (2 * M) as usize]).unwrap();
    w.discard(0, M, DiscardPolicy::RequireDeallocation).unwrap();
    w.flush().unwrap();
    drop(w);
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "vhdx"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vhdx", "-O", "raw"])
            .arg(&path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    let bytes = std::fs::read(raw).unwrap();
    assert!(bytes[..M as usize].iter().all(|b| *b == 0));
    assert!(bytes[M as usize..].iter().all(|b| *b == 7));
}

#[test]
fn fixed_leave_blocks_allocated_profile_refuses_native_release_before_mutation() {
    use virtdisk::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixed.vhdx");
    let w = VhdxWriter::create(&path, M).unwrap();
    w.write_all_at(0, &vec![7; M as usize]).unwrap();
    w.flush().unwrap();
    drop(w);
    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(3 * M + 65540)).unwrap();
    f.write_all(&1u32.to_le_bytes()).unwrap();
    drop(f);
    let before = std::fs::read(&path).unwrap();
    let w = VhdxWriter::open(&path).unwrap();
    assert_eq!(
        w.discard(0, M, DiscardPolicy::RequireDeallocation)
            .unwrap_err()
            .kind(),
        virtdisk::io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
