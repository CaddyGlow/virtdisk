//! Native VDI export with bounded block streaming.
use crate::ReadAt;
use std::{
    fs::OpenOptions,
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};
const BLOCK: u64 = 1024 * 1024;
const MAX_MAP: u64 = 64 * 1024 * 1024;
fn put(b: &mut [u8], at: usize, n: u32) {
    b[at..at + 4].copy_from_slice(&n.to_le_bytes());
}
use crate::native_id::identity;
/// Export an immutable reader to a new dynamic VDI 1.1 image.
///
/// Requires nonempty sector-aligned capacity, at most 16 million 1 MiB blocks.
/// Uses two source passes, at most 64 MiB of map memory and a 64 KiB buffer.
/// Zero blocks are unallocated; the final block is padded with zeroes. Image and
/// modification UUIDs are freshly generated. Existing paths are never replaced.
/// The source must remain immutable throughout both passes. Metadata is written
/// last and the result is synced; failures may leave an incomplete destination.
/// This does not sync the parent directory or provide atomic publication.
pub fn create_vdi(path: impl AsRef<Path>, mut source: &dyn ReadAt) -> io::Result<()> {
    export_vdi(path.as_ref(), &mut source)
}
pub(crate) fn export_vdi(
    path: &Path,
    source: &mut dyn crate::export_source::ExportSource,
) -> io::Result<()> {
    let size = source.size();
    if size == 0 || !size.is_multiple_of(512) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VDI capacity must be positive and sector aligned",
        ));
    }
    let blocks = size.div_ceil(BLOCK);
    let map_bytes = blocks
        .checked_mul(4)
        .filter(|&n| n <= MAX_MAP)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI map exceeds 64 MiB export limit",
            )
        })?;
    let data = (512 + map_bytes).div_ceil(512) * 512;
    let data32 = u32::try_from(data).map_err(|_| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "VDI data offset exceeds format limit",
        )
    })?;
    source.begin(crate::OperationPhase::AllocationScan, size)?;
    let mut map = vec![u32::MAX; blocks as usize];
    let mut buffer = crate::operation_context::scratch_buffer(65536)?;
    let mut allocated = 0u32;
    for (i, entry) in map.iter_mut().enumerate() {
        let offset = i as u64 * BLOCK;
        let end = size.min(offset + BLOCK);
        let mut position = offset;
        let mut nonzero = false;
        while position < end {
            let count = (end - position).min(buffer.len() as u64) as usize;
            source.read(position, &mut buffer[..count])?;
            nonzero |= buffer[..count].iter().any(|&b| b != 0);
            position += count as u64;
        }
        if nonzero {
            *entry = allocated;
            allocated += 1;
        }
    }
    let mut header = [0; 512];
    let banner = b"<<< Oracle VM VirtualBox Disk Image >>>\n";
    header[..banner.len()].copy_from_slice(banner);
    put(&mut header, 64, 0xbeda107f);
    put(&mut header, 68, 0x10001);
    put(&mut header, 72, 400);
    put(&mut header, 76, 1);
    put(&mut header, 340, 512);
    put(&mut header, 344, data32);
    put(&mut header, 360, 512);
    put(&mut header, 468, 512);
    header[368..376].copy_from_slice(&size.to_le_bytes());
    put(&mut header, 376, BLOCK as u32);
    put(&mut header, 384, blocks as u32);
    put(&mut header, 388, allocated);
    header[392..408].copy_from_slice(&identity()?);
    header[408..424].copy_from_slice(&identity()?);
    let final_len = data
        .checked_add(allocated as u64 * BLOCK)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "VDI output size overflow"))?;
    let payload_bytes: u64 = map
        .iter()
        .enumerate()
        .filter(|(_, entry)| **entry != u32::MAX)
        .map(|(index, _)| (size - index as u64 * BLOCK).min(BLOCK))
        .sum();
    source.begin(crate::OperationPhase::ImageExport, payload_bytes)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.set_len(final_len)?;
    for (i, &entry) in map.iter().enumerate() {
        if entry == u32::MAX {
            continue;
        }
        let offset = i as u64 * BLOCK;
        file.seek(SeekFrom::Start(data + entry as u64 * BLOCK))?;
        let end = size.min(offset + BLOCK);
        let mut position = offset;
        while position < end {
            let count = (end - position).min(buffer.len() as u64) as usize;
            source.read(position, &mut buffer[..count])?;
            file.write_all(&buffer[..count])?;
            position += count as u64;
        }
        // create_new plus set_len leaves the final block's unread tail zeroed.
    }
    // Sync data before installing the metadata which makes it reachable.
    file.sync_all()?;
    file.seek(SeekFrom::Start(512))?;
    for chunk in map.chunks(buffer.len() / 4) {
        for (entry, bytes) in chunk.iter().zip(buffer.as_chunks_mut::<4>().0) {
            *bytes = entry.to_le_bytes();
        }
        file.write_all(&buffer[..chunk.len() * 4])?;
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    file.sync_all()
}

/// Create an empty native VDI differencing image over an immutable VDI parent.
///
/// `parent_paths` lists the parent's direct parent through base in order. UUID
/// linkage and capacity are captured from the validated chain; no filenames are
/// stored in VDI. New identities are generated. Existing paths are never replaced.
/// Success syncs the image but not its directory; errors may leave partial output.
pub fn create_vdi_overlay(
    path: impl AsRef<Path>,
    parent_path: impl AsRef<Path>,
    parent_paths: &[std::path::PathBuf],
) -> io::Result<()> {
    create_locked_vdi_overlay(path.as_ref(), parent_path.as_ref(), parent_paths).map(drop)
}

pub(crate) fn create_locked_vdi_overlay(
    path: &Path,
    parent_path: &Path,
    parent_paths: &[std::path::PathBuf],
) -> io::Result<std::fs::File> {
    if parent_paths.len() >= 31 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VDI overlay exceeds chain depth limit",
        ));
    }
    let parent = crate::Vdi::open_chain(parent_path, parent_paths)?;
    let (creation, modification) = parent.identifiers();
    if creation == [0; 16] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VDI parent has no creation identity",
        ));
    }
    let size = parent.len();
    let blocks = size.div_ceil(BLOCK);
    let map_bytes = blocks
        .checked_mul(4)
        .filter(|n| *n <= MAX_MAP)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI overlay map exceeds export limit",
            )
        })?;
    let data = (512 + map_bytes).div_ceil(512) * 512;
    let mut header = [0; 512];
    let banner = b"<<< Oracle VM VirtualBox Disk Image >>>\n";
    header[..banner.len()].copy_from_slice(banner);
    for (at, value) in [
        (64, 0xbeda107f),
        (68, 0x10001),
        (72, 400),
        (76, 4),
        (340, 512),
        (344, data as u32),
        (360, 512),
        (468, 512),
        (376, BLOCK as u32),
        (384, blocks as u32),
    ] {
        put(&mut header, at, value);
    }
    header[368..376].copy_from_slice(&size.to_le_bytes());
    header[392..408].copy_from_slice(&identity()?);
    header[408..424].copy_from_slice(&identity()?);
    header[424..440].copy_from_slice(&creation);
    header[440..456].copy_from_slice(&modification);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    file.set_len(data)?;
    file.seek(SeekFrom::Start(512))?;
    let entries = [0xff; 65536];
    let mut remaining = map_bytes;
    while remaining != 0 {
        let count = remaining.min(entries.len() as u64) as usize;
        file.write_all(&entries[..count])?;
        remaining -= count as u64;
    }
    file.sync_all()?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    file.sync_all()?;
    Ok(file)
}
