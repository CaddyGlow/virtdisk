//! Bounded VirtualBox VDI 1.1 fixed and dynamic image reading.
use crate::{CacheReservation, ParserLimits, ReadAt, ReadBudget, ReadContext, check_range};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
const FREE: u32 = u32::MAX;
const ZERO: u32 = u32::MAX - 1;
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
/// An immutable logical view of a VDI 1.1 fixed or dynamic image.
///
/// Unresolved differencing images, old versions and unknown flags are rejected.
/// The caller must keep the source immutable for this reader's lifetime.
pub struct Vdi {
    source: Arc<dyn ReadAt>,
    length: u64,
    dynamic: bool,
    creation_uuid: [u8; 16],
    modification_uuid: [u8; 16],
    parent: Option<Arc<Vdi>>,
    block: u64,
    extra: u64,
    data: u64,
    map: Vec<u32>,
    budget: ReadBudget,
    _cache: CacheReservation,
}
impl Vdi {
    pub(crate) fn info_profile(&self) -> (u64, bool) {
        (self.block, self.dynamic)
    }
    /// Whether this reader retains an authorized immutable VDI parent.
    pub fn has_parent(&self) -> bool {
        self.parent.is_some()
    }
    /// Explicit path used to open the immediate parent; VDI stores UUIDs rather than filenames.
    pub fn resolved_parent_path(&self) -> Option<PathBuf> {
        self.parent
            .as_ref()
            .and_then(|parent| parent.context().container)
    }
    pub(crate) fn identifiers(&self) -> ([u8; 16], [u8; 16]) {
        (self.creation_uuid, self.modification_uuid)
    }
    pub(crate) fn parent_reader(&self) -> Option<Arc<Vdi>> {
        self.parent.clone()
    }
    /// Open an explicitly ordered direct-parent through base chain.
    /// No parent filenames are inferred from UUIDs. Every file must remain immutable.
    pub fn open_chain(path: impl AsRef<Path>, parent_paths: &[PathBuf]) -> io::Result<Self> {
        Self::open_chain_with_limits(path, parent_paths, ParserLimits::default())
    }
    /// Open an ordered chain with shared caller-tightened parser budgets.
    /// Aliases, cycles, extra ancestors and UUID/geometry mismatches are rejected.
    pub fn open_chain_with_limits(
        path: impl AsRef<Path>,
        parent_paths: &[PathBuf],
        limits: ParserLimits,
    ) -> io::Result<Self> {
        let raw = Arc::new(crate::RawDisk::open(path)?);
        let identity = raw.identity()?;
        Self::open_parent_paths(raw, parent_paths, ReadBudget::new(limits)?, Some(&identity))
    }
    pub(crate) fn open_locked_chain(
        source: Arc<dyn ReadAt>,
        parent_paths: &[PathBuf],
        identity: &same_file::Handle,
    ) -> io::Result<Self> {
        Self::open_parent_paths(
            source,
            parent_paths,
            ReadBudget::new(ParserLimits::default())?,
            Some(identity),
        )
    }
    fn open_parent_paths(
        source: Arc<dyn ReadAt>,
        parent_paths: &[PathBuf],
        budget: ReadBudget,
        identity: Option<&same_file::Handle>,
    ) -> io::Result<Self> {
        if parent_paths.len() >= 32
            || parent_paths.len() as u64 + 1 > budget.limits().recursion_depth
        {
            return Err(invalid("VDI chain exceeds recursion limit"));
        }
        let mut files = Vec::new();
        let mut identities = Vec::new();
        budget.metadata(parent_paths.len() as u64 * 128)?;
        for path in parent_paths {
            budget.work(1)?;
            let raw = Arc::new(crate::RawDisk::open(path)?);
            let candidate = raw.identity()?;
            if identity.is_some_and(|id| id == &candidate) || identities.contains(&candidate) {
                return Err(invalid("VDI chain contains aliased images"));
            }
            identities.push(candidate);
            files.push(raw);
        }
        let mut parent = None;
        for file in files.into_iter().rev() {
            parent = Some(Arc::new(Self::parse(file, budget.clone(), parent)?));
        }
        Self::parse(source, budget, parent)
    }
    /// Validate and open a fixed or dynamic VDI using default parser limits.
    pub fn open(source: Arc<dyn ReadAt>) -> io::Result<Self> {
        Self::open_with_limits(source, ParserLimits::default())
    }
    /// Validate and open a VDI with caller-tightened metadata and work limits.
    pub fn open_with_limits(source: Arc<dyn ReadAt>, limits: ParserLimits) -> io::Result<Self> {
        Self::parse(source, ReadBudget::new(limits)?, None)
    }
    fn parse(
        source: Arc<dyn ReadAt>,
        budget: ReadBudget,
        parent: Option<Arc<Vdi>>,
    ) -> io::Result<Self> {
        if let Some(path) = source.context().container
            && crate::transaction::pending(&path)?
        {
            return Err(invalid(
                "VDI has a pending transaction; reopen with VdiWriter to recover",
            ));
        }
        let source = budget.reader(source);
        budget.metadata(472)?;
        let mut header = [0; 472];
        source.read_exact_at(0, &mut header)?;
        if u32le(&header, 64) != 0xbeda107f {
            return Err(invalid("invalid VDI signature"));
        }
        if u32le(&header, 68) != 0x10001 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported VDI version",
            ));
        }
        let header_size = u32le(&header, 72) as u64;
        if header_size != 400 && header_size != 416 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported VDI header size",
            ));
        }
        check_range(72, header_size, source.len())?;
        let kind = u32le(&header, 76);
        if kind != 1 && kind != 2 && kind != 4 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI differencing and undo images are unsupported",
            ));
        }
        if u32le(&header, 80) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported VDI flags",
            ));
        }
        // Oracle's vdiValidateHeader rejects both nil native image identities.
        // This applies to standalone and differencing images; a nil modification
        // UUID is not a valid initial epoch. No UUID version bits are prescribed.
        if header[392..408].iter().all(|&byte| byte == 0) {
            return Err(invalid("VDI creation UUID is nil"));
        }
        if header[408..424].iter().all(|&byte| byte == 0) {
            return Err(invalid("VDI modification UUID is nil"));
        }
        if kind == 4 {
            let parent = parent
                .as_ref()
                .ok_or_else(|| invalid("VDI differencing image requires an authorized parent"))?;
            if header[424..440].iter().all(|b| *b == 0)
                || header[424..440] != parent.creation_uuid
                || header[440..456] != parent.modification_uuid
            {
                return Err(invalid("VDI parent creation or modification UUID mismatch"));
            }
            if header[392..408] == parent.creation_uuid {
                return Err(invalid("VDI child duplicates parent creation UUID"));
            }
        } else if parent.is_some() || header[424..456].iter().any(|&b| b != 0) {
            return Err(invalid(
                "standalone VDI has unexpected parent or identifiers",
            ));
        }
        let map_offset = u32le(&header, 340) as u64;
        let data = u32le(&header, 344) as u64;
        let sector = u32le(&header, 360);
        let length = u64::from_le_bytes(header[368..376].try_into().unwrap());
        let block = u32le(&header, 376) as u64;
        let extra = u32le(&header, 380) as u64;
        let count = u32le(&header, 384) as u64;
        let allocated = u32le(&header, 388) as u64;
        if sector != 512
            || block < 512
            || !block.is_power_of_two()
            || (extra != 0 && (!extra.is_power_of_two() || !extra.is_multiple_of(512)))
        {
            return Err(invalid("invalid VDI sector or allocation geometry"));
        }
        if length == 0
            || !length.is_multiple_of(512)
            || count != length.div_ceil(block)
            || allocated > count
        {
            return Err(invalid("inconsistent VDI capacity or allocation count"));
        }
        if let Some(parent) = &parent
            && length != parent.length
        {
            return Err(invalid("VDI parent capacity mismatch"));
        }
        let map_bytes = count * 4;
        if map_offset < 72 + header_size
            || map_offset
                .checked_add(map_bytes)
                .is_none_or(|end| end > data)
        {
            return Err(invalid("VDI metadata regions overlap"));
        }
        check_range(map_offset, map_bytes, source.len())?;
        let stride = block + extra;
        let physical_bytes = allocated
            .checked_mul(stride)
            .ok_or_else(|| invalid("VDI allocation extent overflow"))?;
        check_range(data, physical_bytes, source.len())?;
        budget.metadata(map_bytes)?;
        let cache = budget.cache(map_bytes)?;
        let mut encoded =
            vec![0; usize::try_from(map_bytes).map_err(|_| invalid("VDI map too large"))?];
        source.read_exact_at(map_offset, &mut encoded)?;
        budget.work(count)?;
        budget.metadata(map_bytes)?;
        let transient_cache = budget.cache(map_bytes)?;
        let map: Vec<u32> = encoded
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect();
        drop(encoded);
        drop(transient_cache);
        // A dense reverse ownership bitmap avoids adversarial hash-table work.
        budget.metadata(allocated)?;
        let ownership_cache = budget.cache(allocated)?;
        let mut owned = vec![false; allocated as usize];
        let mut seen = 0u64;
        for &entry in &map {
            if entry == FREE || entry == ZERO {
                if kind == 2 {
                    return Err(invalid("fixed VDI contains unallocated blocks"));
                }
            } else {
                if entry as u64 >= allocated || owned[entry as usize] {
                    return Err(invalid("invalid or multiply owned VDI allocation"));
                }
                owned[entry as usize] = true;
                seen += 1;
            }
        }
        if seen != allocated {
            return Err(invalid("VDI allocation count includes unowned blocks"));
        }
        drop(owned);
        drop(ownership_cache);
        Ok(Self {
            source,
            length,
            dynamic: kind != 2,
            creation_uuid: header[392..408].try_into().unwrap(),
            modification_uuid: header[408..424].try_into().unwrap(),
            parent,
            block,
            extra,
            data,
            map,
            budget,
            _cache: cache,
        })
    }
}
impl ReadAt for Vdi {
    fn len(&self) -> u64 {
        self.length
    }
    fn visit_extents(
        &self,
        visitor: &mut dyn FnMut(crate::DiskExtent) -> io::Result<()>,
    ) -> io::Result<()> {
        for (index, entry) in self.map.iter().enumerate() {
            self.budget.work(1)?;
            let offset = index as u64 * self.block;
            visitor(crate::DiskExtent {
                offset,
                length: (self.length - offset).min(self.block),
                kind: if *entry == FREE && self.parent.is_some() {
                    crate::ExtentKind::Inherited
                } else if *entry == FREE || *entry == ZERO {
                    crate::ExtentKind::Zero
                } else {
                    crate::ExtentKind::Allocated
                },
            })?;
        }
        Ok(())
    }
    fn context(&self) -> ReadContext {
        self.source.context()
    }
    fn budget(&self) -> Option<ReadBudget> {
        Some(self.budget.clone())
    }
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        let result = (|| {
            check_range(offset, destination.len() as u64, self.length)?;
            let mut offset = offset;
            let mut remaining = destination;
            while !remaining.is_empty() {
                self.budget.work(1)?;
                let within = offset % self.block;
                let count = (self.block - within).min(remaining.len() as u64) as usize;
                let (out, next) = remaining.split_at_mut(count);
                let entry = self.map[(offset / self.block) as usize];
                if entry == FREE {
                    if let Some(parent) = &self.parent {
                        parent.read_exact_at(offset, out)?;
                    } else {
                        out.fill(0);
                    }
                } else if entry == ZERO {
                    out.fill(0);
                } else {
                    self.source.read_exact_at(
                        self.data + entry as u64 * (self.block + self.extra) + self.extra + within,
                        out,
                    )?;
                }
                offset += count as u64;
                remaining = next;
            }
            Ok(())
        })();
        result.map_err(|e| {
            let mut context = self.context();
            context.offset = Some(offset);
            context.error("read VDI", e)
        })
    }
}
