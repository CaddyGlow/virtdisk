#![cfg(feature = "std")]
use std::sync::Arc;
use virtdisk::io;
use virtdisk::{ParserLimits, RawDisk, ReadAt, Vhdx, create_vhdx};
const M: usize = 1 << 20;
#[path = "support/bytes.rs"]
mod bytes;
use bytes::Bytes;

fn put(b: &mut [u8], o: usize, n: u32) {
    b[o..o + 4].copy_from_slice(&n.to_le_bytes());
}
fn put64(b: &mut [u8], o: usize, n: u64) {
    b[o..o + 8].copy_from_slice(&n.to_le_bytes());
}
fn crc(b: &mut [u8]) {
    put(b, 4, 0);
    let mut c = !0u32;
    for &v in b.iter() {
        c ^= v as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    put(b, 4, !c);
}
fn base() -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk");
    create_vhdx(&path, &Bytes(vec![0x44; M])).unwrap();
    std::fs::read(path).unwrap()
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
    put64(&mut b, 48, 5 * M as u64);
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
    let mut b = base();
    let original = b[2 * M..2 * M + 4096].to_vec();
    let e = entry(
        if data { Some(&original) } else { None },
        1,
        0,
        2 * M as u64,
        5 * M as u64,
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
fn data_and_zero_replay_do_not_modify_source() {
    for data in [true, false] {
        let b = dirty(data);
        assert!(Vhdx::open(Arc::new(Bytes(b.clone()))).is_err());
        let disk = Vhdx::open_recovered(Arc::new(Bytes(b.clone()))).unwrap();
        let mut out = [0; 512];
        disk.read_exact_at(0, &mut out).unwrap();
        assert_eq!(out, [if data { 0x44 } else { 0 }; 512]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk");
        std::fs::write(&path, &b).unwrap();
        Vhdx::open_recovered(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b);
    }
}
#[test]
fn corrupt_log_guid_sequences_tail_and_truncation_fail() {
    for (at, n) in [
        (4, 0),
        (12, 4096),
        (16, 0),
        (32, 1),
        (64 + 24, 2),
        (4100, 1),
        (8188, 2),
    ] {
        let mut b = dirty(true);
        put(&mut b, M + at, n);
        if at != 4 {
            crc(&mut b[M..M + 8192]);
        }
        assert!(Vhdx::open_recovered(Arc::new(Bytes(b))).is_err(), "{at}");
    }
    let mut b = dirty(true);
    b.truncate(4 * M);
    assert!(Vhdx::open_recovered(Arc::new(Bytes(b))).is_err());
}
#[test]
fn recovery_limits_and_interrupted_reads_are_honored() {
    assert!(
        Vhdx::open_recovered_with_limits(
            Arc::new(Bytes(dirty(true))),
            ParserLimits {
                metadata_bytes: 100,
                ..Default::default()
            }
        )
        .is_err()
    );
    struct Stop;
    impl ReadAt for Stop {
        fn len(&self) -> u64 {
            5 * M as u64
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        }
    }
    assert_eq!(
        Vhdx::open_recovered(Arc::new(Stop)).err().unwrap().kind(),
        io::ErrorKind::Interrupted
    );
}
#[test]
#[ignore = "requires independent qemu-img log replay oracle"]
fn qemu_replays_spec_log_to_same_logical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    for data in [true, false] {
        let b = dirty(data);
        let path = dir.path().join(format!("dirty-{data}.vhdx"));
        let raw = dir.path().join(format!("raw-{data}"));
        std::fs::write(&path, b).unwrap();
        assert!(
            std::process::Command::new("qemu-img")
                .args(["check", "-r", "all", "-f", "vhdx"])
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
        assert_eq!(
            std::fs::read(raw).unwrap(),
            vec![if data { 0x44 } else { 0 }; M]
        );
    }
}

#[test]
fn wrapped_entries_and_newest_complete_sequences_replay_in_order() {
    let mut b = dirty(true);
    // Correct sector contains the original valid BAT mapping.
    let original = base()[2 * M..2 * M + 4096].to_vec();
    let first = entry(
        Some(&original),
        7,
        (M - 4096) as u32,
        2 * M as u64,
        5 * M as u64,
    );
    b[M..2 * M].fill(0);
    b[2 * M - 4096..2 * M].copy_from_slice(&first[..4096]);
    b[M..M + 4096].copy_from_slice(&first[4096..]);
    let second = entry(None, 8, (M - 4096) as u32, 2 * M as u64, 5 * M as u64);
    b[M + 4096..M + 8192].copy_from_slice(&second);
    let disk = Vhdx::open_recovered(Arc::new(Bytes(b))).unwrap();
    let mut out = [9; 512];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0; 512]);
}
#[test]
fn log_is_replayed_before_any_region_or_bat_read_and_extension_is_virtual() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Guard {
        bytes: Bytes,
        log_seen: AtomicBool,
    }
    impl ReadAt for Guard {
        fn len(&self) -> u64 {
            self.bytes.len()
        }
        fn read_exact_at(&self, o: u64, b: &mut [u8]) -> io::Result<()> {
            if o == M as u64 && b.len() == M {
                self.log_seen.store(true, Ordering::SeqCst);
            } else if o >= 196608 {
                assert!(
                    self.log_seen.load(Ordering::SeqCst),
                    "metadata read before replay"
                );
            }
            self.bytes.read_exact_at(o, b)
        }
    }
    let mut b = dirty(true);
    put64(&mut b, M + 56, 6 * M as u64);
    crc(&mut b[M..M + 8192]);
    Vhdx::open_recovered(Arc::new(Guard {
        bytes: Bytes(b),
        log_seen: AtomicBool::new(false),
    }))
    .unwrap();
}

#[test]
#[ignore = "requires independent qemu-img circular-log replay oracle"]
fn qemu_replays_wrapped_multi_entry_log_in_order() {
    let mut b = dirty(true);
    let original = base()[2 * M..2 * M + 4096].to_vec();
    let first = entry(
        Some(&original),
        7,
        (M - 4096) as u32,
        2 * M as u64,
        5 * M as u64,
    );
    b[M..2 * M].fill(0);
    b[2 * M - 4096..2 * M].copy_from_slice(&first[..4096]);
    b[M..M + 4096].copy_from_slice(&first[4096..]);
    let second = entry(None, 8, (M - 4096) as u32, 2 * M as u64, 5 * M as u64);
    b[M + 4096..M + 8192].copy_from_slice(&second);
    let disk = Vhdx::open_recovered(Arc::new(Bytes(b.clone()))).unwrap();
    let mut expected = vec![9; M];
    disk.read_exact_at(0, &mut expected).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wrapped.vhdx");
    let raw = dir.path().join("raw");
    std::fs::write(&path, b).unwrap();
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-r", "all", "-f", "vhdx"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["convert", "-f", "vhdx", "-O", "raw"])
            .arg(path)
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(std::fs::read(raw).unwrap(), expected);
}
