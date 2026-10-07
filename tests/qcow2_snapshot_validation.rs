use std::{io, sync::Arc};
use virtdisk::{Qcow2, ReadAt};
struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let end = start
            .checked_add(out.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        out.copy_from_slice(self.0.get(start..end).ok_or(io::ErrorKind::UnexpectedEof)?);
        Ok(())
    }
}
fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_be_bytes());
}
fn fixture() -> Vec<u8> {
    let mut b = vec![0; 4096];
    b[..4].copy_from_slice(b"QFI\xfb");
    for (at, n) in [
        (4, 3u32),
        (20, 9),
        (36, 1),
        (56, 1),
        (60, 1),
        (96, 4),
        (100, 104),
    ] {
        b[at..at + 4].copy_from_slice(&n.to_be_bytes());
    }
    for (at, n) in [
        (24, 512),
        (40, 512),
        (48, 1024),
        (64, 3584),
        (512, 1536),
        (1024, 2560),
        (1536, 2048),
        (3072, 1536 | (1 << 63)),
        (3584, 3072),
        (3624, 0),
        (3632, 512),
    ] {
        put64(&mut b, at, n);
    }
    b[3592..3596].copy_from_slice(&1u32.to_be_bytes());
    b[3596..3598].copy_from_slice(&1u16.to_be_bytes());
    b[3598..3600].copy_from_slice(&1u16.to_be_bytes());
    b[3620..3624].copy_from_slice(&16u32.to_be_bytes());
    b[3640..3642].copy_from_slice(b"1s");
    for (index, count) in [1u16, 1, 1, 2, 2, 1, 1, 1].into_iter().enumerate() {
        b[2560 + index * 2..2562 + index * 2].copy_from_slice(&count.to_be_bytes());
    }
    b
}
fn validate(b: Vec<u8>) -> io::Result<virtdisk::Qcow2Validation> {
    Qcow2::open(Arc::new(Bytes(b)))?.validate_active_mapping()
}
#[test]
fn shared_snapshot_ownership_counts_every_state_and_ignores_inactive_copied_flags() {
    let stats = validate(fixture()).unwrap();
    assert_eq!(stats.referenced_clusters, 8);
    assert_eq!(stats.data_descriptors, 2);
}
#[test]
fn inactive_private_l2_stale_payload_flags_and_partial_directory_tail_validate() {
    let mut bytes = fixture();
    bytes.resize(4608, 0);
    put64(&mut bytes, 3072, 4096 | (1 << 63));
    put64(&mut bytes, 4096, 2048 | (1 << 63));
    put64(&mut bytes, 4104, 1 << 63);
    put64(&mut bytes, 512, 1536 | (1 << 63));
    bytes[2566..2568].copy_from_slice(&1u16.to_be_bytes());
    bytes[2576..2578].copy_from_slice(&1u16.to_be_bytes());
    assert_eq!(validate(bytes).unwrap().referenced_clusters, 9);
    let mut bytes = fixture();
    bytes.truncate(3642);
    assert_eq!(validate(bytes).unwrap().referenced_clusters, 8);
}

#[test]
fn snapshot_only_corruption_and_vm_state_fail_closed() {
    for (at, data) in [
        (2566, 1u16.to_be_bytes().to_vec()),
        (3072, 1024u64.to_be_bytes().to_vec()),
        (3632, 0u64.to_be_bytes().to_vec()),
        (512, (1536u64 | (1 << 63)).to_be_bytes().to_vec()),
    ] {
        let mut b = fixture();
        b[at..at + data.len()].copy_from_slice(&data);
        assert_eq!(
            validate(b).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "at {at}"
        );
    }
    let mut b = fixture();
    put64(&mut b, 3624, 1);
    assert_eq!(validate(b).unwrap_err().kind(), io::ErrorKind::Unsupported);
}

#[test]
#[ignore = "requires independent qemu-img and qemu-io snapshot ownership oracle"]
fn qemu_native_snapshot_maps_validate_across_refcount_widths_and_compression() {
    use std::{path::Path, process::Command};
    fn run(program: &str, args: &[&str], path: &Path) {
        let out = Command::new(program).args(args).arg(path).output().unwrap();
        assert!(
            out.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let dir = tempfile::tempdir().unwrap();
    for (bits, compatibility) in [
        (2, "1.1"),
        (4, "1.1"),
        (8, "1.1"),
        (16, "1.1"),
        (32, "1.1"),
        (64, "1.1"),
        (16, "0.10"),
    ] {
        let path = dir
            .path()
            .join(format!("snapshot-{bits}-{compatibility}.qcow2"));
        let out = Command::new("qemu-img")
            .args(["create", "-f", "qcow2", "-o"])
            .arg(format!(
                "compat={compatibility},refcount_bits={bits},cluster_size=4096"
            ))
            .arg(&path)
            .arg("128K")
            .output()
            .unwrap();
        assert!(out.status.success());
        run(
            "qemu-io",
            &["-f", "qcow2", "-c", "write -P 0x31 0 8192"],
            &path,
        );
        run("qemu-img", &["snapshot", "-c", "first"], &path);
        run(
            "qemu-io",
            &["-f", "qcow2", "-c", "write -P 0x72 4096 8192"],
            &path,
        );
        run("qemu-img", &["snapshot", "-c", "second"], &path);
        run("qemu-io", &["-f", "qcow2", "-c", "write -z 0 4096"], &path);
        run("qemu-img", &["check"], &path);
        let disk = Qcow2::open(Arc::new(virtdisk::RawDisk::open(&path).unwrap())).unwrap();
        assert_eq!(disk.list_snapshots().unwrap().len(), 2);
        assert!(disk.validate_active_mapping().unwrap().data_descriptors >= 6);
    }
    let raw = dir.path().join("compressed.raw");
    std::fs::write(&raw, vec![0x55; 131072]).unwrap();
    let compressed = dir.path().join("compressed.qcow2");
    let out = Command::new("qemu-img")
        .args(["convert", "-f", "raw", "-O", "qcow2", "-c"])
        .arg(&raw)
        .arg(&compressed)
        .output()
        .unwrap();
    assert!(out.status.success());
    run("qemu-img", &["snapshot", "-c", "compressed"], &compressed);
    run(
        "qemu-io",
        &["-f", "qcow2", "-c", "write -P 0x44 0 512"],
        &compressed,
    );
    run("qemu-img", &["check"], &compressed);
    let disk = Qcow2::open(Arc::new(virtdisk::RawDisk::open(&compressed).unwrap())).unwrap();
    assert!(
        disk.validate_active_mapping_and_compressed_payloads()
            .unwrap()
            .compressed_payloads_verified
            > 0
    );
}

#[test]
fn global_snapshot_walk_honors_cancellation_and_caller_budgets() {
    let disk = Qcow2::open(Arc::new(Bytes(fixture()))).unwrap();
    assert_eq!(
        disk.validate_active_mapping_with_cancel(|| true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    for limits in [
        virtdisk::ParserLimits {
            metadata_bytes: 1,
            ..Default::default()
        },
        virtdisk::ParserLimits {
            cache_bytes: 1,
            ..Default::default()
        },
        virtdisk::ParserLimits {
            work_items: 100,
            ..Default::default()
        },
    ] {
        let disk = Qcow2::open_with_limits(Arc::new(Bytes(fixture())), limits).unwrap();
        assert_eq!(
            disk.validate_active_mapping().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}
