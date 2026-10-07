use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{Qcow2, RawDisk};

/// Create an empty QCOW2 v3 external overlay over an explicitly named parent.
///
/// Only `"raw"` and standalone `"qcow2"` parents are supported. Parent paths are
/// canonicalized and stored as absolute UTF-8 filenames of at most 1023 bytes;
/// protocol-like paths, alternate streams and filenames containing NUL are rejected.
/// QCOW2 parents with additional backing images require a future authorization
/// API and are currently rejected. Parent contents must remain immutable while
/// creating or using children; this function does not lock the parent.
///
/// Capacity must be sector-aligned and at most 1 TiB. Unallocated child clusters
/// inherit the parent, with zero reads beyond a shorter parent's capacity. The
/// parent is opened read-only, and the destination is exclusively locked during
/// creation. Existing destinations are never overwritten. Success syncs image
/// data and metadata, but not the parent directory. Errors can leave a partial
/// destination. This function creates a disk overlay, not a VM memory snapshot.
pub fn create_qcow2_overlay(
    path: impl AsRef<Path>,
    parent_path: impl AsRef<Path>,
    parent_format: &str,
    capacity: u64,
) -> io::Result<()> {
    create_qcow2_overlay_with_chain(path, parent_path, parent_format, capacity, &[])
}

/// Create an external overlay with explicitly authorized ancestral files.
///
/// The same capacity, publication and immutable-parent rules as
/// [`create_qcow2_overlay`] apply. Every embedded ancestor must be listed.
pub fn create_qcow2_overlay_with_chain(
    path: impl AsRef<Path>,
    parent_path: impl AsRef<Path>,
    parent_format: &str,
    capacity: u64,
    authorized_backing_paths: &[PathBuf],
) -> io::Result<()> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    if capacity > 1 << 40 || !capacity.is_multiple_of(512) {
        return Err(invalid(
            "overlay capacity must be sector-aligned and at most 1 TiB",
        ));
    }
    if !matches!(parent_format, "raw" | "qcow2") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported overlay parent format",
        ));
    }
    let supplied = parent_path
        .as_ref()
        .to_str()
        .ok_or_else(|| invalid("parent filename must be UTF-8"))?;
    if forbidden_path(supplied) {
        return Err(invalid("protocol-like or NUL-containing parent path"));
    }
    let parent = parent_path.as_ref().canonicalize()?;
    let name = parent
        .to_str()
        .ok_or_else(|| invalid("parent filename must be UTF-8"))?;
    let name = if cfg!(windows) {
        name.strip_prefix(r"\\?\").unwrap_or(name)
    } else {
        name
    };
    if name.len() > 1023 || forbidden_path(name) {
        return Err(invalid("unsupported canonical parent filename"));
    }
    // Retain the read-only source through publication; callers guarantee that
    // the parent and any external access remain immutable beyond this call.
    let _raw;
    let _qcow;
    if parent_format == "raw" {
        _raw = RawDisk::open(&parent)?;
    } else {
        _qcow = Qcow2::open_chain(&parent, authorized_backing_paths)?;
        _qcow.validate_active_mapping()?;
    }
    const CLUSTER: u64 = 65536;
    let l1_entries = capacity.div_ceil(CLUSTER * (CLUSTER / 8));
    let l1_clusters = l1_entries.div_ceil(CLUSTER / 8);
    let ref_table = 1 + l1_clusters;
    let ref_block = ref_table + 1;
    let total = ref_block + 1;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    file.set_len(total * CLUSTER)?;
    let mut cluster = [0u8; CLUSTER as usize];
    cluster[..4].copy_from_slice(b"QFI\xfb");
    cluster[4..8].copy_from_slice(&3u32.to_be_bytes());
    cluster[8..16].copy_from_slice(&128u64.to_be_bytes());
    cluster[16..20].copy_from_slice(&(name.len() as u32).to_be_bytes());
    cluster[20..24].copy_from_slice(&16u32.to_be_bytes());
    cluster[24..32].copy_from_slice(&capacity.to_be_bytes());
    cluster[36..40].copy_from_slice(&(l1_entries as u32).to_be_bytes());
    cluster[40..48].copy_from_slice(&CLUSTER.to_be_bytes());
    cluster[48..56].copy_from_slice(&(ref_table * CLUSTER).to_be_bytes());
    cluster[56..60].copy_from_slice(&1u32.to_be_bytes());
    cluster[96..100].copy_from_slice(&4u32.to_be_bytes());
    cluster[100..104].copy_from_slice(&104u32.to_be_bytes());
    cluster[104..108].copy_from_slice(&0xe2792acau32.to_be_bytes());
    cluster[108..112].copy_from_slice(&(parent_format.len() as u32).to_be_bytes());
    cluster[112..112 + parent_format.len()].copy_from_slice(parent_format.as_bytes());
    // Format padding ends at byte120, followed by the eight-byte terminator.
    cluster[128..128 + name.len()].copy_from_slice(name.as_bytes());
    file.write_all(&cluster)?;
    cluster.fill(0);
    for _ in 0..l1_clusters {
        file.write_all(&cluster)?;
    }
    cluster[..8].copy_from_slice(&(ref_block * CLUSTER).to_be_bytes());
    file.write_all(&cluster)?;
    cluster.fill(0);
    for index in 0..total as usize {
        cluster[index * 2..index * 2 + 2].copy_from_slice(&1u16.to_be_bytes());
    }
    file.write_all(&cluster)?;
    file.sync_all()
}

fn forbidden_path(name: &str) -> bool {
    let drive_path = cfg!(windows)
        && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name.as_bytes().get(1) == Some(&b':')
        && !name[2..].contains(':')
        && Path::new(name).is_absolute();
    name.contains('\0') || (name.contains(':') && !drive_path)
}
