//! Native hosted-sparse VMDK new-output exporter.
use crate::ReadAt;
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
/// Export an immutable sector-aligned reader as a new monolithic sparse VMDK.
///
/// Existing destinations are never overwritten. Memory is bounded to one 64 KiB
/// grain and a 2 KiB grain table. A failed export can leave a partial output;
/// callers must discard that output. Success synchronizes the new image.
/// This creates a standalone image, without parent or snapshot state.
pub fn create_vmdk(path: impl AsRef<Path>, source: &dyn ReadAt) -> io::Result<()> {
    create_locked_vmdk(path, source, false).map(drop)
}

pub(crate) fn create_locked_vmdk(
    path: impl AsRef<Path>,
    source: &dyn ReadAt,
    fully_allocated: bool,
) -> io::Result<File> {
    create_locked_vmdk_with_parent(path, source, fully_allocated, None)
}
pub(crate) fn create_locked_vmdk_with_parent(
    path: impl AsRef<Path>,
    source: &dyn ReadAt,
    fully_allocated: bool,
    parent: Option<(u32, &str)>,
) -> io::Result<File> {
    let path = path.as_ref();
    let length = source.len();
    if length == 0 || !length.is_multiple_of(512) || length > 1 << 40 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VMDK output requires sector-aligned capacity between 512 bytes and 1 TiB",
        ));
    }
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "VMDK filename must be UTF-8")
    })?;
    if name
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\\')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VMDK filename contains descriptor delimiters",
        ));
    }
    let grains = length.div_ceil(65536);
    let tables = grains.div_ceil(512);
    let gd_sectors = (tables * 4).div_ceil(512);
    let gd = 21;
    let gt = gd + gd_sectors;
    let overhead = (gt + tables * 4).div_ceil(128) * 128;
    let mut cid = [0; 4];
    getrandom::fill(&mut cid).map_err(|e| io::Error::other(e.to_string()))?;
    let cid = u32::from_le_bytes(cid) & 0xfffffffe;
    let (parent_cid, hint) = match parent {
        Some((cid, hint)) => {
            if cid == u32::MAX
                || hint.is_empty()
                || hint.contains([':', '\\', '"'])
                || hint.chars().any(char::is_control)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid VMDK parent hint",
                ));
            }
            (cid, format!("parentFileNameHint=\"{hint}\"\n"))
        }
        None => (u32::MAX, String::new()),
    };
    let descriptor = format!(
        "# Disk DescriptorFile\nversion=1\nCID={cid:08x}\nparentCID={parent_cid:08x}\ncreateType=\"monolithicSparse\"\n{hint}\nRW {} SPARSE \"{}\"\n",
        length / 512,
        name
    );
    if descriptor.len() > 20 * 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VMDK descriptor too large",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    let mut header = [0; 512];
    header[..4].copy_from_slice(b"KDMV");
    put32(&mut header, 4, 1);
    put32(&mut header, 8, 1);
    put64(&mut header, 12, length / 512);
    put64(&mut header, 20, 128);
    put64(&mut header, 28, 1);
    put64(&mut header, 36, 20);
    put32(&mut header, 44, 512);
    put64(&mut header, 56, gd);
    put64(&mut header, 64, overhead);
    header[73..77].copy_from_slice(&[10, 32, 13, 10]);
    // The header remains dirty until all payload and mapping writes are durable.
    header[72] = 1;
    file.write_all(&header)?;
    file.write_all(descriptor.as_bytes())?;
    file.set_len(overhead * 512)?;
    file.seek(SeekFrom::Start(gd * 512))?;
    for t in 0..tables {
        file.write_all(
            &u32::try_from(gt + t * 4)
                .map_err(|_| io::Error::other("VMDK table offset overflow"))?
                .to_le_bytes(),
        )?;
    }
    let mut grain = [0; 65536];
    let mut table = [0; 2048];
    let mut physical = overhead;
    for index in 0..grains {
        let offset = index * 65536;
        let take = (length - offset).min(65536) as usize;
        grain.fill(0);
        source.read_exact_at(offset, &mut grain[..take])?;
        if fully_allocated || grain.iter().any(|b| *b != 0) {
            put32(
                &mut table,
                (index % 512) as usize * 4,
                u32::try_from(physical)
                    .map_err(|_| io::Error::other("VMDK grain offset overflow"))?,
            );
            file.seek(SeekFrom::Start(physical * 512))?;
            file.write_all(&grain)?;
            physical += 128;
        }
        if index % 512 == 511 || index + 1 == grains {
            file.seek(SeekFrom::Start((gt + index / 512 * 4) * 512))?;
            file.write_all(&table)?;
            table.fill(0);
        }
    }
    file.sync_all()?;
    header[72] = 0;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    file.sync_all()?;
    Ok(file)
}
