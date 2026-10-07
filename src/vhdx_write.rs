//! Native clean dynamic VHDX export.
use crate::{
    ReadAt,
    vhdx::{BAT, ID, LOGICAL, META, PARAM, PHYSICAL, SIZE},
};
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};
const M: u64 = 1 << 20;
fn put(b: &mut [u8], o: usize, n: u32) {
    b[o..o + 4].copy_from_slice(&n.to_le_bytes());
}
fn put64(b: &mut [u8], o: usize, n: u64) {
    b[o..o + 8].copy_from_slice(&n.to_le_bytes());
}
pub(crate) fn identity() -> io::Result<[u8; 16]> {
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
    id[7] = (id[7] & 15) | 64;
    id[8] = (id[8] & 63) | 128;
    Ok(id)
}
pub(crate) fn checksum(b: &mut [u8]) {
    put(b, 4, 0);
    let mut crc = !0u32;
    for &v in b.iter() {
        crc ^= v as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    put(b, 4, !crc);
}
/// Export an immutable reader to a new clean standalone dynamic VHDX v1 image.
///
/// Requires positive 512-byte aligned capacity. Uses 1 MiB payload blocks,
/// a BAT bounded to 64 MiB and one 1 MiB streaming buffer. Performs two source
/// passes; callers must keep the source immutable throughout. Logical sectors
/// are 512 bytes and physical sectors 4096 bytes. File, data and disk identities
/// are freshly generated; zero blocks are unallocated. Headers and tables are
/// installed after payloads are synced, followed by a final sync. Existing paths
/// are never replaced. Failure may leave an incomplete destination; the parent
/// directory is not synced and publication is not atomic. This does not perform
/// logged in-place updates of an existing VHDX.
pub fn create_vhdx(path: impl AsRef<Path>, source: &dyn ReadAt) -> io::Result<()> {
    create_impl(path, source.len(), Some(source), None).map(drop)
}
pub(crate) fn create_blank(path: impl AsRef<Path>, size: u64) -> io::Result<File> {
    create_impl(path, size, None, None)
}
fn create_impl(
    path: impl AsRef<Path>,
    size: u64,
    source: Option<&dyn ReadAt>,
    child: Option<ChildMetadata>,
) -> io::Result<File> {
    if size == 0 || !size.is_multiple_of(512) || size > 64 * (1 << 40) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VHDX capacity must be positive, 512-byte aligned and at most 64 TiB",
        ));
    }
    let count = size.div_ceil(M);
    let sector = child.as_ref().map_or(512, |c| c.sector);
    let ratio = (1u64 << 23) * sector as u64 / M;
    let entries = if child.is_some() {
        count.div_ceil(ratio) * (ratio + 1)
    } else {
        count + (count - 1) / ratio
    };
    let bat_bytes = entries
        .checked_mul(8)
        .filter(|&b| b <= 64 * M)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "VHDX export BAT exceeds 64 MiB limit",
            )
        })?;
    let bat_length = bat_bytes.div_ceil(M) * M;
    let meta = 2 * M + bat_length;
    let data = meta + M;
    let mut map = vec![0u64; entries as usize];
    let mut buffer = vec![0; M as usize];
    let mut allocated = 0u64;
    if let Some(source) = source {
        for index in 0..count {
            let offset = index * M;
            let n = (size - offset).min(M) as usize;
            source.read_exact_at(offset, &mut buffer[..n])?;
            if buffer[..n].iter().any(|&v| v != 0) {
                map[(index + index / ratio) as usize] = (data + allocated * M) | 6;
                allocated += 1;
            }
        }
    }
    if let Some(child) = &child {
        let total = 65536
            + child
                .virtual_items
                .iter()
                .map(|(_, _, b)| b.len().max(16).div_ceil(8) * 8)
                .sum::<usize>()
            + child.locator.len()
            + 16;
        if child.virtual_items.len() + 2 > 2047 || total > M as usize {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VHDX child metadata exceeds bounded creation profile",
            ));
        }
    }
    let file_id = identity()?;
    let data_id = identity()?;
    let disk_id = identity()?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    file.set_len(data + allocated * M)?;
    for index in 0..count {
        let entry = map[(index + index / ratio) as usize];
        if entry == 0 {
            continue;
        }
        let offset = index * M;
        let n = (size - offset).min(M) as usize;
        buffer.fill(0);
        source.unwrap().read_exact_at(offset, &mut buffer[..n])?;
        file.seek(SeekFrom::Start(entry & !0xfffff))?;
        file.write_all(&buffer)?;
    }
    file.sync_all()?;
    file.seek(SeekFrom::Start(2 * M))?;
    for chunk in map.chunks(buffer.len() / 8) {
        for (entry, out) in chunk.iter().zip(buffer.as_chunks_mut::<8>().0) {
            *out = entry.to_le_bytes();
        }
        file.write_all(&buffer[..chunk.len() * 8])?;
    }
    buffer.fill(0);
    if let Some(child) = child {
        buffer[..8].copy_from_slice(b"metadata");
        let mut items = vec![(
            PARAM,
            4,
            [(M as u32).to_le_bytes(), 2u32.to_le_bytes()].concat(),
        )];
        items.extend(child.virtual_items);
        items.push((crate::vhdx::parent::ITEM, 4, child.locator));
        if items.len() > 2047 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VHDX child metadata table exceeds native limit",
            ));
        }
        buffer[10..12].copy_from_slice(&(items.len() as u16).to_le_bytes());
        let mut offset = 65536usize;
        for (index, (guid, flags, bytes)) in items.iter().enumerate() {
            let at = 32 + index * 32;
            buffer[at..at + 16].copy_from_slice(guid);
            put(&mut buffer, at + 24, *flags);
            if !bytes.is_empty() {
                if offset
                    .checked_add(bytes.len())
                    .is_none_or(|end| end > M as usize)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "VHDX child metadata exceeds one MiB",
                    ));
                }
                put(&mut buffer, at + 16, offset as u32);
                put(&mut buffer, at + 20, bytes.len() as u32);
                buffer[offset..offset + bytes.len()].copy_from_slice(bytes);
                offset += (bytes.len().max(16)).div_ceil(8) * 8;
            }
        }
    } else {
        buffer[..8].copy_from_slice(b"metadata");
        buffer[10] = 5;
        for (index, (guid, len, flags)) in [
            (PARAM, 8, 4),
            (SIZE, 8, 6),
            (ID, 16, 6),
            (LOGICAL, 4, 6),
            (PHYSICAL, 4, 6),
        ]
        .iter()
        .enumerate()
        {
            let at = 32 + index * 32;
            buffer[at..at + 16].copy_from_slice(guid);
            put(&mut buffer, at + 16, 65536 + index as u32 * 16);
            put(&mut buffer, at + 20, *len);
            put(&mut buffer, at + 24, *flags);
        }
        put(&mut buffer, 65536, M as u32);
        put64(&mut buffer, 65552, size);
        buffer[65568..65584].copy_from_slice(&disk_id);
        put(&mut buffer, 65584, 512);
        put(&mut buffer, 65600, 4096);
    }
    file.seek(SeekFrom::Start(meta))?;
    file.write_all(&buffer)?;
    buffer.fill(0);
    buffer[..4].copy_from_slice(b"regi");
    put(&mut buffer, 8, 2);
    for (index, (guid, offset, len)) in [(BAT, 2 * M, bat_length), (META, meta, M)]
        .iter()
        .enumerate()
    {
        let at = 16 + index * 32;
        buffer[at..at + 16].copy_from_slice(guid);
        put64(&mut buffer, at + 16, *offset);
        put(&mut buffer, at + 24, *len as u32);
        put(&mut buffer, at + 28, 1);
    }
    checksum(&mut buffer[..65536]);
    for offset in [196608, 262144] {
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&buffer[..65536])?;
    }
    buffer.fill(0);
    buffer[..4].copy_from_slice(b"head");
    buffer[16..32].copy_from_slice(&file_id);
    buffer[32..48].copy_from_slice(&data_id);
    buffer[66] = 1;
    put(&mut buffer, 68, M as u32);
    put64(&mut buffer, 72, M);
    for (offset, seq) in [(65536, 1), (131072, 2)] {
        put64(&mut buffer, 8, seq);
        checksum(&mut buffer[..4096]);
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&buffer[..4096])?;
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(b"vhdxfile")?;
    file.sync_all()?;
    Ok(file)
}

struct ChildMetadata {
    sector: u32,
    virtual_items: Vec<crate::vhdx::MetadataItem>,
    locator: Vec<u8>,
}
/// Create a new empty native differencing child, retaining the parent's virtual-disk metadata.
/// The direct parent is explicitly authorized by its argument; deeper parents require the list.
/// Every parent must remain immutable while the child exists. Capacity and sector size are inherited.
/// Paths must be representable as UTF-16; creation uses a relative native locator and create_new.
pub fn create_vhdx_overlay(
    path: impl AsRef<Path>,
    parent_path: impl AsRef<Path>,
    authorized_parent_paths: &[std::path::PathBuf],
) -> io::Result<()> {
    create_child(path.as_ref(), parent_path.as_ref(), authorized_parent_paths).map(drop)
}
pub(crate) fn create_child(
    path: &Path,
    parent_path: &Path,
    authorized_parent_paths: &[std::path::PathBuf],
) -> io::Result<File> {
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_child_at(path, parent_path, authorized_parent_paths, directory)
}
pub(crate) fn create_vhdx_overlay_at(
    path: &Path,
    parent_path: &Path,
    authorized_parent_paths: &[std::path::PathBuf],
    locator_directory: &Path,
) -> io::Result<()> {
    create_child_at(
        path,
        parent_path,
        authorized_parent_paths,
        locator_directory,
    )
    .map(drop)
}
fn create_child_at(
    path: &Path,
    parent_path: &Path,
    authorized_parent_paths: &[std::path::PathBuf],
    locator_directory: &Path,
) -> io::Result<File> {
    let parent_path = std::fs::canonicalize(parent_path)?;
    let parent = crate::Vhdx::open_chain(&parent_path, authorized_parent_paths)?;
    let directory = std::fs::canonicalize(locator_directory)?;
    let common = directory
        .components()
        .zip(parent_path.components())
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "VHDX parent and child require a shared path root",
        ));
    }
    let mut relative = std::path::PathBuf::new();
    for _ in common..directory.components().count() {
        relative.push("..");
    }
    for component in parent_path.components().skip(common) {
        relative.push(component.as_os_str());
    }
    #[cfg(unix)]
    if relative.as_os_str().as_encoded_bytes().contains(&b'\\') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VHDX parent path cannot encode a literal backslash",
        ));
    }
    let relative = relative
        .to_str()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "VHDX parent path is not Unicode",
            )
        })?
        .replace('/', "\\");
    if relative.contains(':') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VHDX relative parent path contains stream syntax",
        ));
    }
    let (items, guid) = parent.child_metadata()?;
    let locator = crate::vhdx::parent::encode(guid, &relative)?;
    let child = ChildMetadata {
        sector: parent.geometry().1,
        virtual_items: items,
        locator,
    };
    create_impl(path, parent.len(), None, Some(child))
}
