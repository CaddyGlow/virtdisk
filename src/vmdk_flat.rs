//! Existing monolithic/split flat descriptor validation under retained file locks.
use crate::RawWriter;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
use crate::vmdk_descriptor::{hexadecimal, sectors};
struct Extent {
    writer: Arc<RawWriter>,
    start: u64,
    length: u64,
    offset: u64,
    identity: same_file::Handle,
}
pub(super) struct Flat {
    extents: Vec<Extent>,
}
impl Flat {
    pub(super) fn container_extents_size(&self) -> Option<u64> {
        self.extents
            .iter()
            .try_fold(0u64, |sum, extent| sum.checked_add(extent.writer.len()))
    }

    fn locate(&self, offset: u64) -> &Extent {
        &self.extents[self
            .extents
            .partition_point(|extent| extent.start <= offset)
            - 1]
    }
    pub(super) fn read_exact_at(&self, mut offset: u64, mut out: &mut [u8]) -> io::Result<()> {
        while !out.is_empty() {
            let extent = self.locate(offset);
            let local = offset - extent.start;
            let take = (extent.length - local).min(out.len() as u64) as usize;
            extent
                .writer
                .read_exact_at(extent.offset + local, &mut out[..take])?;
            offset += take as u64;
            out = &mut out[take..];
        }
        Ok(())
    }
    pub(super) fn write_all_at(&self, mut offset: u64, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let extent = self.locate(offset);
            let local = offset - extent.start;
            let take = (extent.length - local).min(data.len() as u64) as usize;
            extent
                .writer
                .write_all_at(extent.offset + local, &data[..take])?;
            offset += take as u64;
            data = &data[take..];
        }
        Ok(())
    }
    pub(super) fn write_zeroes(&self, mut offset: u64, mut length: u64) -> io::Result<()> {
        while length != 0 {
            let extent = self.locate(offset);
            let local = offset - extent.start;
            let take = (extent.length - local).min(length);
            extent.writer.write_zeroes(extent.offset + local, take)?;
            offset += take;
            length -= take;
        }
        Ok(())
    }
    pub(super) fn flush(&self) -> io::Result<()> {
        for extent in &self.extents {
            extent.writer.flush()?;
        }
        Ok(())
    }
}
pub(super) struct Opened {
    pub(super) descriptor: Arc<RawWriter>,
    pub(super) identity: same_file::Handle,
    pub(super) path: PathBuf,
    pub(super) flat: Flat,
    pub(super) length: u64,
    pub(super) cid_offset: u64,
    pub(super) cid_width: usize,
}
pub(super) fn open_policy(
    path: &Path,
    authorized: &[PathBuf],
    policy: crate::RecoveryPolicy,
) -> io::Result<Opened> {
    if authorized.len() > 256 {
        return Err(unsupported(
            "flat VMDK authorization list exceeds 256 paths",
        ));
    }
    let path = path.canonicalize()?;
    let descriptor = Arc::new(RawWriter::open(&path)?);
    policy.check(crate::transaction::pending(&path)?)?;
    if crate::transaction::pending(&path)? {
        return Err(invalid("flat VMDK descriptor has pending transaction"));
    }
    let identity = descriptor.opened_identity()?;
    #[cfg(target_os = "linux")]
    descriptor.require_single_link_for_journal()?;
    if descriptor.is_empty() || descriptor.len() > 65536 {
        return Err(unsupported("flat VMDK descriptor exceeds 64 KiB"));
    }
    let mut bytes = vec![0; descriptor.len() as usize];
    descriptor.read_exact_at(0, &mut bytes)?;
    let text = crate::vmdk_descriptor::text(&bytes)?;
    let mut properties = crate::vmdk_descriptor::Properties::default();
    let mut version = false;
    let mut parent = false;
    let mut profile = None;
    let mut cid = None;
    let mut specs = Vec::new();
    let mut cursor = 0usize;
    for original in text.split_inclusive('\n') {
        let line = original.trim();
        if line.is_empty() || line.starts_with('#') {
            cursor += original.len();
            continue;
        }
        if let Some((key, value)) = crate::vmdk_descriptor::property(original, cursor) {
            let duplicate = properties.duplicate(key);
            let field_range = value.range;
            let value = value.text;
            match key {
                "version" => {
                    if duplicate || value != "1" {
                        return Err(unsupported("unsupported flat VMDK descriptor version"));
                    }
                    version = true;
                }
                "CID" => {
                    hexadecimal(value)?;
                    if duplicate {
                        return Err(invalid("duplicate flat VMDK CID"));
                    }
                    cid = Some((field_range.start as u64, field_range.len()));
                }
                "parentCID" => {
                    if duplicate || hexadecimal(value)? != u32::MAX {
                        return Err(unsupported("flat VMDK parents are not writable"));
                    }
                    parent = true;
                }
                "createType" => {
                    if duplicate
                        || !["\"monolithicFlat\"", "\"twoGbMaxExtentFlat\""].contains(&value)
                    {
                        return Err(unsupported(
                            "unsupported writable flat VMDK descriptor profile",
                        ));
                    }
                    profile = Some(value);
                }
                key if key.starts_with("ddb.") => {}
                _ => return Err(unsupported("unsupported flat VMDK descriptor property")),
            }
        } else {
            if specs.len() >= 256 {
                return Err(unsupported("flat VMDK extent count exceeds 256"));
            }
            let extent = crate::vmdk_descriptor::extent(line, 0)?;
            if extent.access != "RW" || extent.kind != "FLAT" {
                return Err(unsupported("flat VMDK writer requires RW FLAT extent"));
            }
            let length = sectors(extent.count.text)?;
            let name = extent.name;
            if name.is_empty() || name.contains([':', '\\']) || name.chars().any(char::is_control) {
                return Err(unsupported("unsupported flat VMDK extent filename"));
            }
            let offset = sectors(extent.tail)?;
            if length == 0 {
                return Err(invalid("empty flat VMDK extent"));
            }
            specs.push((length, offset, name));
        }
        cursor += original.len();
    }
    if !version || !parent || profile.is_none() || specs.is_empty() {
        return Err(invalid("incomplete flat VMDK descriptor"));
    }
    let (cid_offset, cid_width) = cid.ok_or_else(|| invalid("flat VMDK descriptor lacks CID"))?;
    let split = profile == Some("\"twoGbMaxExtentFlat\"");
    if !split && specs.len() != 1 {
        return Err(invalid("monolithicFlat requires one extent"));
    }
    let mut length = 0u64;
    for (size, _, _) in &specs {
        if split && *size > 2 * 1024 * 1024 * 1024 {
            return Err(unsupported("split flat VMDK extent exceeds 2 GiB"));
        }
        length = length
            .checked_add(*size)
            .ok_or_else(|| invalid("flat VMDK capacity overflow"))?;
    }
    super::VmdkWriter::check_size(length)?;
    let mut approved = std::collections::BTreeSet::new();
    for candidate in authorized {
        approved.insert(candidate.canonicalize()?);
    }
    let mut extents: Vec<Extent> = Vec::with_capacity(specs.len());
    let mut start = 0u64;
    for (size, offset, name) in specs {
        let extent_path = path
            .parent()
            .ok_or_else(|| invalid("missing flat VMDK directory"))?
            .join(name)
            .canonicalize()?;
        if !approved.contains(&extent_path) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "flat VMDK extent requires explicit authorization",
            ));
        }
        let writer = Arc::new(RawWriter::open(&extent_path)?);
        policy.check(crate::transaction::pending(&extent_path)?)?;
        if crate::transaction::pending(&extent_path)? {
            return Err(invalid("flat VMDK extent has pending transaction"));
        }
        let extent_identity = writer.opened_identity()?;
        if extent_identity == identity
            || extents
                .iter()
                .any(|extent| extent.identity == extent_identity)
        {
            return Err(unsupported("flat VMDK extent aliases another opened file"));
        }
        #[cfg(target_os = "linux")]
        writer.require_single_link_for_journal()?;
        crate::check_range(offset, size, writer.len())?;
        extents.push(Extent {
            writer,
            start,
            length: size,
            offset,
            identity: extent_identity,
        });
        start += size;
    }
    Ok(Opened {
        descriptor,
        identity,
        path,
        length,
        cid_offset,
        cid_width,
        flat: Flat { extents },
    })
}
