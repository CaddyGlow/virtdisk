use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

use crate::ReadAt;

const CLUSTER: u64 = 65536;
const ENTRIES: u64 = CLUSTER / 8;
const REFCOUNTS: u64 = CLUSTER / 2;
const COPIED: u64 = 1 << 63;

/// Export a reader to a new standalone QCOW2 v3 image with 64 KiB clusters.
///
/// Uses 16-bit reference counts and allocates every guest cluster, including
/// zero-filled clusters. Memory consumption is bounded to two clusters. Sources
/// must remain immutable throughout export. Capacities not divisible by 512 bytes or above 1 TiB are rejected
/// before creating the destination. Existing destinations are never overwritten.
///
/// On error a partial destination can remain and must not be used. Successful
/// return syncs image contents and metadata, but not its parent directory. This
/// operation provides creation/export, not in-place writable QCOW2 access.
pub fn create_qcow2(path: impl AsRef<Path>, source: &dyn ReadAt) -> io::Result<()> {
    create_locked_qcow2(path, source).map(drop)
}

pub(crate) fn create_locked_qcow2(path: impl AsRef<Path>, source: &dyn ReadAt) -> io::Result<File> {
    create_locked(path.as_ref(), source, false)
}

/// Export a standalone QCOW2 v3 image, omitting entirely zero payload clusters.
///
/// Scans the immutable source twice using two 64 KiB buffers. Capacity must be
/// sector-aligned and at most 1 TiB. Metadata tables remain fully allocated.
/// Existing destinations are never overwritten; errors can leave a partial file.
/// Success syncs the image but not its parent directory.
pub fn create_sparse_qcow2(path: impl AsRef<Path>, source: &dyn ReadAt) -> io::Result<()> {
    create_locked(path.as_ref(), source, true).map(drop)
}

pub(crate) fn create_locked_sparse_qcow2(path: &Path, source: &dyn ReadAt) -> io::Result<File> {
    create_locked(path, source, true)
}

fn create_locked(path: &Path, mut source: &dyn ReadAt, sparse: bool) -> io::Result<File> {
    export_qcow2(path, &mut source, sparse)
}
pub(crate) fn export_qcow2(
    path: &Path,
    source: &mut dyn crate::export_source::ExportSource,
    sparse: bool,
) -> io::Result<File> {
    let size = source.size();
    if size > 1 << 40 || !size.is_multiple_of(512) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "QCOW2 export requires sector-aligned capacity no larger than 1 TiB",
        ));
    }
    let guest_clusters = size.div_ceil(CLUSTER);
    let mut data_clusters = guest_clusters;
    if sparse {
        source.begin(crate::OperationPhase::AllocationScan, size)?;
        data_clusters = 0;
        let mut buffer = [0; CLUSTER as usize];
        for index in 0..guest_clusters {
            let count = (size - index * CLUSTER).min(CLUSTER) as usize;
            source.read(index * CLUSTER, &mut buffer[..count])?;
            if buffer[..count].iter().any(|b| *b != 0) {
                data_clusters += 1;
            }
        }
    }
    source.begin(crate::OperationPhase::ImageExport, size)?;
    let l2_clusters = guest_clusters.div_ceil(ENTRIES);
    let l1_clusters = l2_clusters.div_ceil(ENTRIES);
    let base = 1 + l1_clusters + l2_clusters + data_clusters;
    let mut refblocks = 1;
    let mut reftables = 1;
    loop {
        let total = base + refblocks + reftables;
        let next_blocks = total.div_ceil(REFCOUNTS);
        let next_tables = next_blocks.div_ceil(ENTRIES);
        if next_blocks == refblocks && next_tables == reftables {
            break;
        }
        refblocks = next_blocks;
        reftables = next_tables;
    }
    let total = base + refblocks + reftables;
    let l1_start = 1;
    let reftable_start = l1_start + l1_clusters;
    let refblock_start = reftable_start + reftables;
    let l2_start = refblock_start + refblocks;
    let data_start = l2_start + l2_clusters;
    let file_size = total
        .checked_mul(CLUSTER)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "QCOW2 layout overflow"))?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    file.set_len(file_size)?;
    let mut buffer = [0u8; CLUSTER as usize];
    buffer[..4].copy_from_slice(b"QFI\xfb");
    buffer[4..8].copy_from_slice(&3u32.to_be_bytes());
    buffer[20..24].copy_from_slice(&16u32.to_be_bytes());
    buffer[24..32].copy_from_slice(&size.to_be_bytes());
    buffer[36..40].copy_from_slice(&(l2_clusters as u32).to_be_bytes());
    buffer[40..48].copy_from_slice(&(l1_start * CLUSTER).to_be_bytes());
    buffer[48..56].copy_from_slice(&(reftable_start * CLUSTER).to_be_bytes());
    buffer[56..60].copy_from_slice(&(reftables as u32).to_be_bytes());
    buffer[96..100].copy_from_slice(&4u32.to_be_bytes());
    buffer[100..104].copy_from_slice(&104u32.to_be_bytes());
    file.write_all(&buffer)?;
    for table in 0..l1_clusters {
        buffer.fill(0);
        for entry in 0..ENTRIES {
            let index = table * ENTRIES + entry;
            if index >= l2_clusters {
                break;
            }
            put_entry(&mut buffer, entry, ((l2_start + index) * CLUSTER) | COPIED);
        }
        file.seek(SeekFrom::Start((l1_start + table) * CLUSTER))?;
        file.write_all(&buffer)?;
    }
    for table in 0..reftables {
        buffer.fill(0);
        for entry in 0..ENTRIES {
            let index = table * ENTRIES + entry;
            if index >= refblocks {
                break;
            }
            put_entry(&mut buffer, entry, (refblock_start + index) * CLUSTER);
        }
        file.seek(SeekFrom::Start((reftable_start + table) * CLUSTER))?;
        file.write_all(&buffer)?;
    }
    for block in 0..refblocks {
        buffer.fill(0);
        for index in 0..REFCOUNTS.min(total - block * REFCOUNTS) {
            let position = index as usize * 2;
            buffer[position..position + 2].copy_from_slice(&1u16.to_be_bytes());
        }
        file.seek(SeekFrom::Start((refblock_start + block) * CLUSTER))?;
        file.write_all(&buffer)?;
    }
    let mut allocated = 0;
    let mut payload = [0; CLUSTER as usize];
    for table in 0..l2_clusters {
        buffer.fill(0);
        for entry in 0..ENTRIES {
            let index = table * ENTRIES + entry;
            if index >= guest_clusters {
                break;
            }
            payload.fill(0);
            let count = (size - index * CLUSTER).min(CLUSTER) as usize;
            source.read(index * CLUSTER, &mut payload[..count])?;
            if sparse && payload[..count].iter().all(|b| *b == 0) {
                continue;
            }
            if allocated >= data_clusters {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source allocation changed during export",
                ));
            }
            let physical = (data_start + allocated) * CLUSTER;
            put_entry(&mut buffer, entry, physical | COPIED);
            file.seek(SeekFrom::Start(physical))?;
            file.write_all(&payload)?;
            allocated += 1;
        }
        file.seek(SeekFrom::Start((l2_start + table) * CLUSTER))?;
        file.write_all(&buffer)?;
    }
    if allocated != data_clusters {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source allocation changed during export",
        ));
    }
    file.sync_all()?;
    Ok(file)
}

fn put_entry(buffer: &mut [u8], entry: u64, value: u64) {
    let position = entry as usize * 8;
    buffer[position..position + 8].copy_from_slice(&value.to_be_bytes());
}
