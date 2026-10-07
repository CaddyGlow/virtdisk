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
fn fixture(version: u32) -> Vec<u8> {
    let mut image = vec![0; 4096];
    image[..4].copy_from_slice(b"QFI\xfb");
    image[4..8].copy_from_slice(&version.to_be_bytes());
    image[20..24].copy_from_slice(&9u32.to_be_bytes());
    image[24..32].copy_from_slice(&1536u64.to_be_bytes());
    image[36..40].copy_from_slice(&1u32.to_be_bytes());
    image[40..48].copy_from_slice(&512u64.to_be_bytes());
    image[48..56].copy_from_slice(&1024u64.to_be_bytes());
    image[56..60].copy_from_slice(&1u32.to_be_bytes());
    if version == 3 {
        image[96..100].copy_from_slice(&4u32.to_be_bytes());
        image[100..104].copy_from_slice(&104u32.to_be_bytes());
    }
    image[512..520].copy_from_slice(&1536u64.to_be_bytes());
    image[1536..1544].copy_from_slice(&2048u64.to_be_bytes());
    image[2048..2560].fill(0x5a);
    image
}

#[test]
fn reads_allocated_and_sparse_clusters_across_boundary() {
    for version in [2, 3] {
        let disk = Qcow2::open(Arc::new(Bytes(fixture(version)))).unwrap();
        let mut out = [0xff; 16];
        disk.read_exact_at(504, &mut out).unwrap();
        assert_eq!(
            out,
            [
                0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert!(disk.read_exact_at(1536, &mut []).is_ok());
        assert!(disk.read_exact_at(1536, &mut [0]).is_err());
        assert!(disk.read_exact_at(u64::MAX, &mut [0]).is_err());
    }
}

#[test]
fn zero_flag_suppresses_preallocated_payload() {
    let mut image = fixture(3);
    image[1536..1544].copy_from_slice(&2049u64.to_be_bytes());
    let disk = Qcow2::open(Arc::new(Bytes(image))).unwrap();
    let mut out = [1; 32];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0; 32]);
}

#[test]
fn unsupported_and_malformed_headers_fail_closed() {
    for (offset, bytes) in [
        (72, 1u64.to_be_bytes().to_vec()),
        (8, 104u64.to_be_bytes().to_vec()),
        (32, 1u32.to_be_bytes().to_vec()),
        (40, 513u64.to_be_bytes().to_vec()),
        (36, 0u32.to_be_bytes().to_vec()),
        (100, 103u32.to_be_bytes().to_vec()),
    ] {
        let mut image = fixture(3);
        image[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(
            Qcow2::open(Arc::new(Bytes(image))).is_err(),
            "field {offset}"
        );
    }
}

#[test]
fn invalid_or_unsupported_mapping_returns_an_error() {
    for entry in [
        1u64 << 62,
        2048 | 2,
        3584 + 512,
        2048 | (1 << 56),
        2048 + 512 + 16,
    ] {
        let mut image = fixture(3);
        image[1536..1544].copy_from_slice(&entry.to_be_bytes());
        let disk = Qcow2::open(Arc::new(Bytes(image))).unwrap();
        assert!(disk.read_exact_at(0, &mut [0]).is_err(), "entry {entry}");
    }
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn randomized_ranges_match_qemu_converted_raw_image() {
    use std::{fs, process::Command};
    use virtdisk::RawDisk;
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source.raw");
    let mut expected = vec![0; 1024 * 1024 + 777];
    let mut state = 7u64;
    for (i, byte) in expected.iter_mut().enumerate() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        if i / 65536 % 3 == 0 {
            *byte = (state >> 32) as u8;
        }
    }
    fs::write(&raw, &expected).unwrap();
    let raw_before = source_digest(&raw);
    for compatibility in ["0.10", "1.1"] {
        let image = directory
            .path()
            .join(format!("image-{compatibility}.qcow2"));
        assert!(
            Command::new("qemu-img")
                .args(["convert", "-f", "raw", "-O", "qcow2", "-o"])
                .arg(format!("compat={compatibility},cluster_size=4096"))
                .arg(&raw)
                .arg(&image)
                .status()
                .unwrap()
                .success()
        );
        let before = source_digest(&image);
        let disk = Qcow2::open(Arc::new(RawDisk::open(&image).unwrap())).unwrap();
        disk.validate_active_mapping().unwrap();
        // QEMU rounds the virtual disk length to a sector boundary.
        expected.resize(disk.len() as usize, 0);
        for _ in 0..1000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let offset = state as usize % expected.len();
            let count = (expected.len() - offset).min(8193);
            let mut out = vec![0; count];
            disk.read_exact_at(offset as u64, &mut out).unwrap();
            assert_eq!(out, expected[offset..offset + count]);
        }
        assert_eq!(source_digest(&image), before);
    }
    assert_eq!(source_digest(&raw), raw_before);
}

fn source_digest(path: &std::path::Path) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    digest.finalize().into()
}

fn compressed_fixture(payload: &[u8], zstd: bool) -> Vec<u8> {
    let mut image = fixture(3);
    let offset = 2061usize;
    image[offset..offset + payload.len()].copy_from_slice(payload);
    let sectors = (offset % 512 + payload.len()).div_ceil(512);
    let descriptor = (1u64 << 62) | (((sectors - 1) as u64) << 61) | offset as u64;
    image[1536..1544].copy_from_slice(&descriptor.to_be_bytes());
    if zstd {
        image[72..80].copy_from_slice(&8u64.to_be_bytes());
        image[100..104].copy_from_slice(&112u32.to_be_bytes());
        image[104] = 1;
    }
    image
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn packed_deflate_cluster_roundtrips_and_rejects_wrong_output_lengths() {
    for size in [511, 512, 513, 8192] {
        let image = compressed_fixture(&deflate(&vec![0x39; size]), false);
        let disk = Qcow2::open(Arc::new(Bytes(image))).unwrap();
        let mut out = [0; 23];
        let result = disk.read_exact_at(17, &mut out);
        if size == 512 {
            result.unwrap();
            assert_eq!(out, [0x39; 23]);
        } else {
            assert!(result.is_err(), "decoded size {size}");
        }
    }
}

#[test]
fn zstd_cluster_decodes_single_bounded_frame() {
    // Single-segment frame, two-byte content length (512 - 256), one raw block.
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x60, 0x00, 0x01, 0x01, 0x10, 0x00];
    frame.extend_from_slice(&[0x72; 512]);
    let disk = Qcow2::open(Arc::new(Bytes(compressed_fixture(&frame, true)))).unwrap();
    let mut out = [0; 17];
    disk.read_exact_at(495, &mut out).unwrap();
    assert_eq!(out, [0x72; 17]);
}

#[test]
fn compressed_descriptors_reject_copied_reserved_offsets_and_truncation() {
    for entry in [
        (1u64 << 63) | (1 << 62) | 2048,
        (1 << 62) | 4095,
        (1 << 62) | (1 << 56),
    ] {
        let mut image = fixture(3);
        image[1536..1544].copy_from_slice(&entry.to_be_bytes());
        let disk = Qcow2::open(Arc::new(Bytes(image))).unwrap();
        assert!(disk.read_exact_at(0, &mut [0]).is_err());
    }
}

fn set_backing(image: &mut [u8], name: &str, format: &str) {
    image[8..16].copy_from_slice(&256u64.to_be_bytes());
    image[16..20].copy_from_slice(&(name.len() as u32).to_be_bytes());
    image[256..256 + name.len()].copy_from_slice(name.as_bytes());
    image[104..108].copy_from_slice(&0xe2792acau32.to_be_bytes());
    image[108..112].copy_from_slice(&(format.len() as u32).to_be_bytes());
    image[112..112 + format.len()].copy_from_slice(format.as_bytes());
}

#[test]
fn backing_requires_authorization_and_zero_overrides_a_short_raw_parent() {
    use std::fs;
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("base.raw");
    fs::write(&base, vec![0x41; 700]).unwrap();
    let image_path = root.path().join("child.qcow2");
    let mut image = fixture(3);
    image[1536..1544].fill(0);
    image[1552..1560].copy_from_slice(&1u64.to_be_bytes());
    set_backing(&mut image, "base.raw", "raw");
    fs::write(&image_path, &image).unwrap();
    assert!(Qcow2::open(Arc::new(virtdisk::RawDisk::open(&image_path).unwrap())).is_err());
    let error = Qcow2::open_chain(&image_path, &[]).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let disk = Qcow2::open_chain(&image_path, &[base]).unwrap();
    let mut out = vec![0xff; 1536];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(&out[..700], &[0x41; 700]);
    assert!(out[700..].iter().all(|byte| *byte == 0));
}

#[test]
fn cycles_and_malformed_backing_headers_fail_closed() {
    use std::fs;
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a.qcow2");
    let b = root.path().join("b.qcow2");
    let mut image = fixture(3);
    set_backing(&mut image, "b.qcow2", "qcow2");
    fs::write(&a, &image).unwrap();
    set_backing(&mut image, "a.qcow2", "qcow2");
    fs::write(&b, &image).unwrap();
    assert!(Qcow2::open_chain(&a, &[a.clone(), b]).is_err());
    for (offset, value) in [
        (16, 1024u32.to_be_bytes().to_vec()),
        (8, 511u64.to_be_bytes().to_vec()),
    ] {
        let mut image = fixture(3);
        set_backing(&mut image, "a.qcow2", "qcow2");
        image[offset..offset + value.len()].copy_from_slice(&value);
        assert!(Qcow2::open(Arc::new(Bytes(image))).is_err());
    }
}

#[test]
#[ignore = "requires independent qemu-img and qemu-io oracles"]
fn qemu_compression_and_three_layer_backing_chain_match_raw_oracle() {
    use std::{fs, process::Command};
    use virtdisk::RawDisk;
    let root = tempfile::tempdir().unwrap();
    let raw = root.path().join("base.raw");
    let mut expected = vec![0; 2 * 1024 * 1024];
    for (i, byte) in expected.iter_mut().enumerate() {
        *byte = (i / 4096 + i % 19) as u8;
    }
    fs::write(&raw, &expected).unwrap();
    for (algorithm, cluster_size) in ["zlib", "zstd"]
        .into_iter()
        .flat_map(|algorithm| [512, 4096, 65536, 2097152].map(|cluster| (algorithm, cluster)))
    {
        let compressed = root
            .path()
            .join(format!("{algorithm}-{cluster_size}.qcow2"));
        assert!(
            Command::new("qemu-img")
                .args(["convert", "-c", "-f", "raw", "-O", "qcow2", "-o"])
                .arg(format!(
                    "compression_type={algorithm},cluster_size={cluster_size}"
                ))
                .arg(&raw)
                .arg(&compressed)
                .status()
                .unwrap()
                .success()
        );
        let before = source_digest(&compressed);
        let raw_before = source_digest(&raw);
        let disk = Qcow2::open(Arc::new(RawDisk::open(&compressed).unwrap())).unwrap();
        let report = disk
            .validate_active_mapping_and_compressed_payloads()
            .unwrap();
        assert_eq!(report.containers, 1);
        assert!(report.compressed_clusters > 0);
        assert_eq!(
            report.compressed_clusters,
            report.compressed_payloads_verified
        );
        let mut output = vec![0; expected.len()];
        disk.read_exact_at(0, &mut output).unwrap();
        assert_eq!(output, expected, "{algorithm}");
        assert_eq!(source_digest(&compressed), before);
        assert_eq!(source_digest(&raw), raw_before);
    }
    let middle = root.path().join("middle.qcow2");
    let top = root.path().join("top.qcow2");
    for (child, parent, format) in [(&middle, &raw, "raw"), (&top, &middle, "qcow2")] {
        assert!(
            Command::new("qemu-img")
                .args(["create", "-f", "qcow2", "-F", format, "-b"])
                .arg(parent)
                .arg(child)
                .status()
                .unwrap()
                .success()
        );
    }
    assert!(
        Command::new("qemu-io")
            .args(["-f", "qcow2", "-c", "write -P 0x66 8192 6000"])
            .arg(&middle)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("qemu-io")
            .args([
                "-f",
                "qcow2",
                "-c",
                "write -z 0 4096",
                "-c",
                "write -P 0x77 100000 8193"
            ])
            .arg(&top)
            .status()
            .unwrap()
            .success()
    );
    let oracle = root.path().join("oracle.raw");
    assert!(
        Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&top)
            .arg(&oracle)
            .status()
            .unwrap()
            .success()
    );
    let expected = fs::read(&oracle).unwrap();
    let sources = [&top, &middle, &raw, &oracle];
    let before = sources.map(|path| source_digest(path));
    let disk = Qcow2::open_chain(&top, &[middle.clone(), raw.clone()]).unwrap();
    assert_eq!(disk.validate_active_mapping().unwrap().containers, 2);
    let mut state = 47u64;
    for _ in 0..2000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let offset = state as usize % expected.len();
        let count = (expected.len() - offset).min(8193);
        let mut output = vec![0; count];
        disk.read_exact_at(offset as u64, &mut output).unwrap();
        assert_eq!(output, expected[offset..offset + count]);
    }
    assert_eq!(sources.map(|path| source_digest(path)), before);
}

#[test]
#[ignore = "requires independent qemu-img and qemu-io oracles"]
fn qemu_v2_compressed_backing_chain_matches_raw_oracle() {
    use std::{fs, process::Command};
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("base.raw");
    let changed = root.path().join("changed.raw");
    let middle = root.path().join("compressed-v2.qcow2");
    let top = root.path().join("top-v2.qcow2");
    let oracle = root.path().join("oracle.raw");
    let mut bytes = vec![0x31; 2 * 1024 * 1024];
    fs::write(&base, &bytes).unwrap();
    bytes[4096..8192].fill(0x65);
    bytes[16384..20480].fill(0);
    fs::write(&changed, &bytes).unwrap();
    assert!(
        Command::new("qemu-img")
            .args([
                "convert",
                "-c",
                "-f",
                "raw",
                "-O",
                "qcow2",
                "-o",
                "compat=0.10,cluster_size=4096",
                "-F",
                "raw",
                "-B"
            ])
            .arg(&base)
            .arg(&changed)
            .arg(&middle)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "qcow2",
                "-o",
                "compat=0.10,cluster_size=4096",
                "-F",
                "qcow2",
                "-b"
            ])
            .arg(&middle)
            .arg(&top)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("qemu-io")
            .args(["-f", "qcow2", "-c", "write -P 0x77 65537 8193"])
            .arg(&top)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(&top)
            .arg(&oracle)
            .status()
            .unwrap()
            .success()
    );
    for path in [&top, &middle] {
        let header = fs::read(path).unwrap();
        assert_eq!(u32::from_be_bytes(header[4..8].try_into().unwrap()), 2);
    }
    let paths = [&base, &changed, &middle, &top, &oracle];
    let before = paths.map(|path| source_digest(path));
    let disk = Qcow2::open_chain(&top, &[middle.clone(), base.clone()]).unwrap();
    let report = disk
        .validate_active_mapping_and_compressed_payloads()
        .unwrap();
    assert_eq!(report.containers, 2);
    assert!(report.compressed_clusters > 0);
    assert_eq!(
        report.compressed_clusters,
        report.compressed_payloads_verified
    );
    let expected = fs::read(&oracle).unwrap();
    let mut full = vec![0; expected.len()];
    disk.read_exact_at(0, &mut full).unwrap();
    assert_eq!(full, expected);
    let mut state = 53u64;
    for _ in 0..2000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let offset = state as usize % expected.len();
        let count = (expected.len() - offset).min(8193);
        let mut actual = vec![0; count];
        disk.read_exact_at(offset as u64, &mut actual).unwrap();
        assert_eq!(actual, expected[offset..offset + count]);
    }
    assert_eq!(paths.map(|path| source_digest(path)), before);
}

#[test]
fn chain_depth_and_protocol_paths_are_rejected() {
    use std::fs;
    let root = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for i in 0..33 {
        let path = root.path().join(format!("{i}.qcow2"));
        let mut image = fixture(3);
        if i < 32 {
            set_backing(&mut image, &format!("{}.qcow2", i + 1), "qcow2");
        }
        fs::write(&path, image).unwrap();
        paths.push(path);
    }
    assert!(Qcow2::open_chain(&paths[0], &paths[1..]).is_err());
    for name in ["https://example.org/disk", "file:base", "disk:stream"] {
        let path = root.path().join("protocol.qcow2");
        let mut image = fixture(3);
        set_backing(&mut image, name, "raw");
        fs::write(&path, image).unwrap();
        assert_eq!(
            Qcow2::open_chain(path, &[]).err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
    }
}

#[cfg(unix)]
#[test]
fn hardlink_and_symlink_aliases_do_not_bypass_cycle_detection() {
    use std::{fs, os::unix::fs::symlink};
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.qcow2");
    let alias = root.path().join("alias.qcow2");
    let mut image = fixture(3);
    set_backing(&mut image, "alias.qcow2", "qcow2");
    fs::write(&original, image).unwrap();
    fs::hard_link(&original, &alias).unwrap();
    assert!(Qcow2::open_chain(&original, std::slice::from_ref(&alias)).is_err());
    let link = root.path().join("link.qcow2");
    symlink(&original, &link).unwrap();
    assert!(Qcow2::open_chain(&link, &[alias]).is_err());
}

#[test]
fn zstd_checksum_mismatch_and_excessive_window_are_rejected() {
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x64, 0x00, 0x01, 0x01, 0x10, 0x00];
    frame.extend_from_slice(&[0x72; 512]);
    frame.extend_from_slice(&[0; 4]); // deliberately wrong checksum
    let disk = Qcow2::open(Arc::new(Bytes(compressed_fixture(&frame, true)))).unwrap();
    assert!(disk.read_exact_at(0, &mut [0]).is_err());
    // A non-single-segment frame declares a 1 GiB window before any blocks.
    let frame = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0xa0];
    let disk = Qcow2::open(Arc::new(Bytes(compressed_fixture(&frame, true)))).unwrap();
    assert!(disk.read_exact_at(0, &mut [0]).is_err());
}

fn validated_fixture() -> Vec<u8> {
    let mut image = fixture(3);
    image[1024..1032].copy_from_slice(&3072u64.to_be_bytes());
    for cluster in [0, 1, 2, 3, 4, 6] {
        image[3072 + cluster * 2..3074 + cluster * 2].copy_from_slice(&1u16.to_be_bytes());
    }
    image[512..520].copy_from_slice(&(1536u64 | (1 << 63)).to_be_bytes());
    image[1536..1544].copy_from_slice(&(2048u64 | (1 << 63)).to_be_bytes());
    image
}

#[test]
fn l1_allocated_tail_padding_is_not_required_at_physical_eof() {
    let mut image = validated_fixture();
    image[40..48].copy_from_slice(&3584u64.to_be_bytes());
    image[3584..3592].copy_from_slice(&(1536u64 | (1 << 63)).to_be_bytes());
    image[3074..3076].copy_from_slice(&0u16.to_be_bytes());
    image[3086..3088].copy_from_slice(&1u16.to_be_bytes());
    image.truncate(3592);
    let disk = Qcow2::open(Arc::new(Bytes(image.clone()))).unwrap();
    disk.validate_active_mapping().unwrap();
    let mut out = [0; 8];
    disk.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, [0x5a; 8]);
    image.truncate(3591);
    assert!(Qcow2::open(Arc::new(Bytes(image))).is_err());
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn qemu_blank_large_image_with_partial_l1_tail_validates() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("blank.qcow2");
    assert!(
        std::process::Command::new("qemu-img")
            .args(["create", "-f", "qcow2"])
            .arg(&path)
            .arg("64G")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let before = source_digest(&path);
    let disk = Qcow2::open_chain(&path, &[]).unwrap();
    disk.validate_active_mapping_and_compressed_payloads()
        .unwrap();
    let mut out = [1; 8];
    disk.read_exact_at(64 * 1024 * 1024 * 1024 - 8, &mut out)
        .unwrap();
    assert_eq!(out, [0; 8]);
    assert_eq!(source_digest(&path), before);
}

#[test]
fn active_validator_checks_metadata_ownership_refcounts_and_cancellation() {
    let image = validated_fixture();
    let disk = Qcow2::open(Arc::new(Bytes(image.clone()))).unwrap();
    let report = disk.validate_active_mapping().unwrap();
    assert_eq!(report.containers, 1);
    assert_eq!(report.metadata_extents, 5);
    assert_eq!(report.data_descriptors, 1);
    assert_eq!(report.compressed_clusters, 0);
    assert_eq!(
        disk.validate_active_mapping_with_cancel(|| true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    for (offset, value) in [
        (40, 1024u64.to_be_bytes().to_vec()), // L1 overlaps refcount table
        (1024, 1024u64.to_be_bytes().to_vec()), // refcount block overlaps table
        (1536, 512u64.to_be_bytes().to_vec()), // data points into L1 metadata
        (3072, 0u16.to_be_bytes().to_vec()),  // header cluster has zero refcount
        (3080, 0u16.to_be_bytes().to_vec()),  // data cluster has zero refcount
        (3080, 2u16.to_be_bytes().to_vec()),  // copied data has multiple owners
        (3082, 1u16.to_be_bytes().to_vec()),  // leaked unreferenced allocation
        (1536, 2048u64.to_be_bytes().to_vec()), // exclusive data lacks copied flag
    ] {
        let mut broken = image.clone();
        broken[offset..offset + value.len()].copy_from_slice(&value);
        let disk = Qcow2::open(Arc::new(Bytes(broken))).unwrap();
        assert!(
            disk.validate_active_mapping().is_err(),
            "corruption at {offset}"
        );
    }
    let mut bitmap = image.clone();
    bitmap[104..108].copy_from_slice(&0x23852875u32.to_be_bytes());
    let disk = Qcow2::open(Arc::new(Bytes(bitmap))).unwrap();
    assert_eq!(
        disk.validate_active_mapping().unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    let mut snapshot = image;
    snapshot[60..64].copy_from_slice(&1u32.to_be_bytes());
    let disk = Qcow2::open(Arc::new(Bytes(snapshot))).unwrap();
    assert_eq!(
        disk.validate_active_mapping().unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn compressed_payload_audit_checks_descriptors_outside_file_capture() {
    let mut image = validated_fixture();
    let payload = deflate(&vec![0x39; 512]);
    image[1536..1544].copy_from_slice(&((1u64 << 62) | 2061).to_be_bytes());
    image[2061..2061 + payload.len()].copy_from_slice(&payload);
    let disk = Qcow2::open(Arc::new(Bytes(image.clone()))).unwrap();
    let report = disk
        .validate_active_mapping_and_compressed_payloads()
        .unwrap();
    assert_eq!(report.compressed_payloads_verified, 1);
    assert_eq!(report.compressed_bytes_verified, 512);
    image[2061..2560].fill(0xff);
    let disk = Qcow2::open(Arc::new(Bytes(image))).unwrap();
    disk.validate_active_mapping().unwrap();
    assert!(
        disk.validate_active_mapping_and_compressed_payloads()
            .is_err()
    );
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn shared_l2_and_data_ownership_matches_qemu_checker() {
    use std::{fs, process::Command};
    let root = tempfile::tempdir().unwrap();
    for shared_table in [false, true] {
        let mut image = validated_fixture();
        image[1536..1544].copy_from_slice(&2048u64.to_be_bytes());
        image[3080..3082].copy_from_slice(&2u16.to_be_bytes());
        if shared_table {
            image[24..32].copy_from_slice(&33280u64.to_be_bytes());
            image[36..40].copy_from_slice(&2u32.to_be_bytes());
            image[512..520].copy_from_slice(&1536u64.to_be_bytes());
            image[520..528].copy_from_slice(&1536u64.to_be_bytes());
            image[3078..3080].copy_from_slice(&2u16.to_be_bytes());
        } else {
            image[1544..1552].copy_from_slice(&2048u64.to_be_bytes());
        }
        let path = root.path().join(format!("shared-{shared_table}.qcow2"));
        fs::write(&path, &image).unwrap();
        let before = source_digest(&path);
        let output = Command::new("qemu-img")
            .args(["check", "-f", "qcow2"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Qcow2::open(Arc::new(Bytes(image.clone())))
            .unwrap()
            .validate_active_mapping()
            .unwrap();
        // Nonzero but incorrect counts must fail independently of the copied flag.
        image[3080..3082].copy_from_slice(&3u16.to_be_bytes());
        let disk = Qcow2::open(Arc::new(Bytes(image))).unwrap();
        assert!(disk.validate_active_mapping().is_err());
        assert_eq!(source_digest(&path), before);
    }
}

#[test]
#[ignore = "requires independent qemu-img oracle"]
fn all_refcount_widths_match_qemu_generated_images() {
    use std::{fs, process::Command};
    let root = tempfile::tempdir().unwrap();
    let raw = root.path().join("source.raw");
    let expected = vec![0x3a; 131072];
    fs::write(&raw, &expected).unwrap();
    for bits in [1, 2, 4, 8, 16, 32, 64] {
        let output = root.path().join(format!("refcount-{bits}.qcow2"));
        assert!(
            Command::new("qemu-img")
                .args(["convert", "-f", "raw", "-O", "qcow2", "-o"])
                .arg(format!("refcount_bits={bits},cluster_size=4096"))
                .arg(&raw)
                .arg(&output)
                .status()
                .unwrap()
                .success()
        );
        let before = source_digest(&output);
        let raw_before = source_digest(&raw);
        let disk = Qcow2::open(Arc::new(virtdisk::RawDisk::open(&output).unwrap())).unwrap();
        disk.validate_active_mapping().unwrap();
        let mut out = vec![0; expected.len()];
        disk.read_exact_at(0, &mut out).unwrap();
        assert_eq!(out, expected, "refcount width {bits}");
        assert_eq!(source_digest(&output), before);
        assert_eq!(source_digest(&raw), raw_before);
    }
}

#[test]
fn caller_tightened_work_and_decompression_limits_stop_real_reads() {
    let limits = virtdisk::ParserLimits {
        work_items: 1,
        ..Default::default()
    };
    let error = Qcow2::open_with_limits(Arc::new(Bytes(fixture(3))), limits)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("work"));
    let image = compressed_fixture(&deflate(&vec![0x39; 512]), false);
    let limits = virtdisk::ParserLimits {
        decompression_buffer_bytes: 511,
        ..Default::default()
    };
    let disk = Qcow2::open_with_limits(Arc::new(Bytes(image)), limits).unwrap();
    let error = disk.read_exact_at(0, &mut [0; 1]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("decompression-buffer"));
}

#[test]
fn validator_rejects_caller_tightened_metadata_and_cache_allocations() {
    for (name, limits) in [
        (
            "metadata",
            virtdisk::ParserLimits {
                metadata_bytes: 1,
                ..Default::default()
            },
        ),
        (
            "cache",
            virtdisk::ParserLimits {
                cache_bytes: 1,
                ..Default::default()
            },
        ),
    ] {
        let disk = Qcow2::open_with_limits(Arc::new(Bytes(validated_fixture())), limits).unwrap();
        let error = disk.validate_active_mapping().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains(name), "{error}");
        assert!(error.get_ref().unwrap().is::<virtdisk::ReadError>());
    }
}

#[test]
fn caller_chain_recursion_limit_rejects_a_valid_authorized_parent() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("parent.qcow2");
    let child = root.path().join("child.qcow2");
    std::fs::write(&parent, fixture(3)).unwrap();
    let mut bytes = fixture(3);
    set_backing(&mut bytes, "parent.qcow2", "qcow2");
    std::fs::write(&child, &bytes).unwrap();
    Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    let limits = virtdisk::ParserLimits {
        recursion_depth: 1,
        ..Default::default()
    };
    let error = Qcow2::open_chain_with_limits(&child, &[parent], limits)
        .err()
        .unwrap();
    assert!(error.to_string().contains("depth limit"));
    assert_eq!(std::fs::read(child).unwrap(), bytes);
}

#[test]
fn immutable_mapping_cache_avoids_repeated_table_reads_and_remains_bounded() {
    let input = fixture(3);
    let disk = Qcow2::open(Arc::new(Bytes(input.clone()))).unwrap();
    let budget = disk.budget().unwrap();
    let mut byte = [0];
    disk.read_exact_at(0, &mut byte).unwrap();
    let before = budget.usage();
    for _ in 0..100 {
        disk.read_exact_at(0, &mut byte).unwrap();
        assert_eq!(byte, [0x5a]);
    }
    let after = budget.usage();
    assert_eq!(after.work_items - before.work_items, 100);
    assert_eq!(after.cache_bytes, before.cache_bytes);
    assert_eq!(after.metadata_bytes, before.metadata_bytes);
    assert_eq!(input, fixture(3));
    drop(disk);
    assert_eq!(budget.usage().cache_bytes, 0);
    let limits = virtdisk::ParserLimits {
        cache_bytes: 128,
        ..Default::default()
    };
    let disk = Qcow2::open_with_limits(Arc::new(Bytes(fixture(3))), limits).unwrap();
    assert_eq!(
        disk.read_exact_at(0, &mut byte).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    let mut malformed = fixture(3);
    malformed[1536..1544].copy_from_slice(&(2048u64 | (1 << 61)).to_be_bytes());
    let disk = Qcow2::open(Arc::new(Bytes(malformed))).unwrap();
    for _ in 0..2 {
        assert_eq!(
            disk.read_exact_at(0, &mut byte).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(disk.budget().unwrap().usage().cache_bytes, 128); // Only valid L1 cached.
    }
}

#[test]
fn warmed_zero_mappings_still_exhaust_tightened_work_limits() {
    for explicit_zero in [false, true] {
        let mut input = fixture(3);
        if explicit_zero {
            input[1536..1544].copy_from_slice(&1u64.to_be_bytes());
        }
        let disk = Qcow2::open_with_limits(
            Arc::new(Bytes(input)),
            virtdisk::ParserLimits {
                work_items: 32,
                ..Default::default()
            },
        )
        .unwrap();
        let mut byte = [255];
        let offset = if explicit_zero { 0 } else { 512 };
        disk.read_exact_at(offset, &mut byte).unwrap();
        assert_eq!(byte, [0]);
        let mut error = None;
        for _ in 0..40 {
            if let Err(e) = disk.read_exact_at(offset, &mut byte) {
                error = Some(e);
                break;
            }
            assert_eq!(byte, [0]);
        }
        let error = error.expect("cached zero reads must remain bounded");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("work"));
        assert_eq!(disk.budget().unwrap().usage().work_items, 32);
    }
}
