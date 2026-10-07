//! Bounded native VHDX readers and explicitly authorized differencing chains.
pub(crate) mod log;
pub(crate) mod parent;
use crate::{CacheReservation, ParserLimits, ReadAt, ReadBudget, ReadContext, check_range};
use std::{collections::BTreeSet, io, sync::Arc};
const M: u64 = 1 << 20;
pub(crate) const BAT: [u8; 16] = [
    0x66, 0x77, 0xc2, 0x2d, 0x23, 0xf6, 0, 0x42, 0x9d, 0x64, 0x11, 0x5e, 0x9b, 0xfd, 0x4a, 8,
];
pub(crate) const META: [u8; 16] = [
    6, 0xa2, 0x7c, 0x8b, 0x90, 0x47, 0x9a, 0x4b, 0xb8, 0xfe, 0x57, 0x5f, 5, 0xf, 0x88, 0x6e,
];
pub(crate) const PARAM: [u8; 16] = [
    0x37, 0x67, 0xa1, 0xca, 0x36, 0xfa, 0x43, 0x4d, 0xb3, 0xb6, 0x33, 0xf0, 0xaa, 0x44, 0xe7, 0x6b,
];
pub(crate) const SIZE: [u8; 16] = [
    0x24, 0x42, 0xa5, 0x2f, 0x1b, 0xcd, 0x76, 0x48, 0xb2, 0x11, 0x5d, 0xbe, 0xd8, 0x3b, 0xf4, 0xb8,
];
pub(crate) const ID: [u8; 16] = [
    0xab, 0x12, 0xca, 0xbe, 0xe6, 0xb2, 0x23, 0x45, 0x93, 0xef, 0xc3, 9, 0xe0, 0, 0xc7, 0x46,
];
pub(crate) const LOGICAL: [u8; 16] = [
    0x1d, 0xbf, 0x41, 0x81, 0x6f, 0xa9, 9, 0x47, 0xba, 0x47, 0xf2, 0x33, 0xa8, 0xfa, 0xab, 0x5f,
];
pub(crate) const PHYSICAL: [u8; 16] = [
    0xc7, 0x48, 0xa3, 0xcd, 0x5d, 0x44, 0x71, 0x44, 0x9c, 0xc9, 0xe9, 0x88, 0x52, 0x51, 0xc5, 0x56,
];
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
fn unsupported(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, s)
}
fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
pub(crate) fn checksum(b: &[u8]) -> bool {
    let mut crc = !0u32;
    for (i, &v) in b.iter().enumerate() {
        crc ^= if (4..8).contains(&i) { 0 } else { v as u32 };
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    !crc == u32le(b, 4)
}
fn read(source: &dyn ReadAt, budget: &ReadBudget, o: u64, n: usize) -> io::Result<Vec<u8>> {
    budget.metadata(n as u64)?;
    let mut b = vec![0; n];
    source.read_exact_at(o, &mut b)?;
    Ok(b)
}
fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}
fn region_table(b: &[u8]) -> bool {
    &b[..4] == b"regi" && checksum(b) && u32le(b, 8) <= 2047 && u32le(b, 12) == 0
}
/// Immutable reader for VHDX v1 fixed, dynamic and authorized differencing images.
///
/// Use explicit chain APIs for parents and recovered APIs for native redo logs.
/// Undefined and discarded standalone blocks are read as zero, as permitted by
/// MS-VHDX. Keep the underlying source immutable throughout deferred reads.
pub struct Vhdx {
    source: Arc<dyn ReadAt>,
    length: u64,
    block: u64,
    bat_offset: u64,
    logical_sector: u32,
    physical_sector: u32,
    map: Vec<u64>,
    states: Vec<u8>,
    bitmaps: Vec<u64>,
    parent: Option<Arc<Vhdx>>,
    locator: Option<parent::Locator>,
    data_guid: [u8; 16],
    metadata_offset: u64,
    bat_length: u64,
    size_offset: u64,
    leave_blocks_allocated: bool,
    budget: ReadBudget,
    _cache: CacheReservation,
}
pub(crate) type MetadataItem = ([u8; 16], u32, Vec<u8>);
type WriterParts = (
    u64,
    u64,
    Vec<u64>,
    ReadBudget,
    CacheReservation,
    Option<Vhdx>,
);
impl Vhdx {
    /// Whether this image declares a native parent locator.
    pub fn has_parent(&self) -> bool {
        self.locator.is_some()
    }
    /// Canonical direct parent path retained by an explicitly authorized chain open.
    pub fn resolved_parent_path(&self) -> Option<std::path::PathBuf> {
        self.parent.as_ref().and_then(|p| p.context().container)
    }
    pub(crate) fn child_metadata(&self) -> io::Result<(Vec<MetadataItem>, [u8; 16])> {
        let _temporary = self.budget.cache(2 * M)?;
        let metadata = read(
            self.source.as_ref(),
            &self.budget,
            self.metadata_offset,
            65536,
        )?;
        let mut copied = Vec::new();
        let mut total = 65536u64;
        for entry in metadata[32..32 + u16le(&metadata, 10) as usize * 32]
            .as_chunks::<32>()
            .0
        {
            let flags = u32le(entry, 24);
            if flags & 2 != 0 {
                let start = u32le(entry, 16) as u64;
                let len = u32le(entry, 20) as u64;
                total = total
                    .checked_add(len.max(16).div_ceil(8) * 8)
                    .ok_or_else(|| invalid("VHDX metadata total overflow"))?;
                if total > M - 65536 {
                    return Err(unsupported(
                        "VHDX child metadata exceeds bounded creation profile",
                    ));
                }
                let bytes = read(
                    self.source.as_ref(),
                    &self.budget,
                    self.metadata_offset + start,
                    len as usize,
                )?;
                copied.push((entry[..16].try_into().unwrap(), flags, bytes));
            }
        }
        Ok((copied, self.data_guid))
    }
    pub(crate) fn bitmap_state(&self) -> (&[u8], &[u64]) {
        (&self.states, &self.bitmaps)
    }
    pub(crate) fn direct_parent(&self) -> Option<Arc<Vhdx>> {
        self.parent.clone()
    }
    pub(crate) fn validate_writable_view(
        source: Arc<dyn ReadAt>,
        budget: ReadBudget,
        parent: Option<Arc<Vhdx>>,
    ) -> io::Result<()> {
        let mut image = Self::parse(source, budget, parent.is_some())?;
        if let Some(parent) = parent {
            let locator = image
                .locator
                .as_ref()
                .ok_or_else(|| invalid("missing retained child locator"))?;
            if !locator.linkage.contains(&parent.data_guid)
                || image.length != parent.length
                || image.logical_sector != parent.logical_sector
            {
                return Err(invalid("changed retained VHDX parent linkage"));
            }
            image.parent = Some(parent);
        }
        Ok(())
    }
    pub(crate) fn requires_copy(&self, index: usize, physical: u64) -> bool {
        self.map[index] == physical
            && (self.states[index] == 7 || (self.states[index] == 0 && self.parent.is_some()))
    }
    pub(crate) fn leave_blocks_allocated(&self) -> bool {
        self.leave_blocks_allocated
    }
    pub(crate) fn bat_offset(&self) -> u64 {
        self.bat_offset
    }
    pub(crate) fn resize_geometry(&self) -> (u64, u64) {
        (self.bat_length, self.size_offset)
    }
    pub(crate) fn geometry(&self) -> (u64, u32, u32, u64) {
        (
            self.length,
            self.logical_sector,
            self.physical_sector,
            self.block,
        )
    }
    pub(crate) fn into_writable_parts(self) -> io::Result<WriterParts> {
        if self.parent.is_some() {
            let cache = self.budget.cache(self.map.len() as u64 * 8)?;
            Ok((
                self.length,
                self.block,
                self.map.clone(),
                self.budget.clone(),
                cache,
                Some(self),
            ))
        } else {
            Ok((
                self.length,
                self.block,
                self.map,
                self.budget,
                self._cache,
                None,
            ))
        }
    }
    /// Open a clean standalone VHDX using default parser limits.
    pub fn open(source: Arc<dyn ReadAt>) -> io::Result<Self> {
        Self::open_with_limits(source, ParserLimits::default())
    }
    /// Open a clean standalone VHDX with tightened parser budgets.
    pub fn open_with_limits(source: Arc<dyn ReadAt>, limits: ParserLimits) -> io::Result<Self> {
        Self::open_with_budget(source, ReadBudget::new(limits)?)
    }
    /// Open an immutable recovered view, replaying the active native log before metadata reads.
    /// The source is never modified; callers must keep it immutable. Recovery is bounded by
    /// the same metadata, cache and cumulative work budgets used for deferred reads.
    pub fn open_recovered(source: Arc<dyn ReadAt>) -> io::Result<Self> {
        Self::open_recovered_with_limits(source, ParserLimits::default())
    }
    /// Open an immutable recovered view using caller-tightened shared parser limits.
    pub fn open_recovered_with_limits(
        source: Arc<dyn ReadAt>,
        limits: ParserLimits,
    ) -> io::Result<Self> {
        let (image, _) = Self::recovered_parts(source, limits)?;
        Ok(image)
    }
    pub(crate) fn recovered_parts(
        source: Arc<dyn ReadAt>,
        limits: ParserLimits,
    ) -> io::Result<(Self, Arc<log::Overlay>)> {
        let budget = ReadBudget::new(limits)?;
        let overlay = log::recover(source, budget.clone())?;
        let image = Self::open_with_budget(overlay.clone(), budget)?;
        overlay.reject_payload_updates(&image.map, image.block)?;
        Ok((image, overlay))
    }
    pub(crate) fn open_with_budget(
        source: Arc<dyn ReadAt>,
        budget: ReadBudget,
    ) -> io::Result<Self> {
        Self::parse(source, budget, false)
    }
    fn parse(source: Arc<dyn ReadAt>, budget: ReadBudget, allow_parent: bool) -> io::Result<Self> {
        let source = budget.reader(source);
        check_range(0, M, source.len())?;
        // Bound transient header/table buffers, region indexes and metadata indexes.
        let _scratch = budget.cache(512 * 1024)?;
        if read(source.as_ref(), &budget, 0, 8)? != b"vhdxfile" {
            return Err(invalid("invalid VHDX file identifier"));
        }
        let h1 = read(source.as_ref(), &budget, 65536, 4096)?;
        let h2 = read(source.as_ref(), &budget, 131072, 4096)?;
        let valid = |b: &[u8]| &b[..4] == b"head" && checksum(b);
        let header = match (valid(&h1), valid(&h2)) {
            (true, true) => {
                if u64le(&h1, 8) == u64le(&h2, 8) {
                    return Err(invalid("VHDX headers have ambiguous sequence numbers"));
                }
                if u64le(&h1, 8) > u64le(&h2, 8) {
                    &h1
                } else {
                    &h2
                }
            }
            (true, false) => &h1,
            (false, true) => &h2,
            _ => return Err(invalid("no valid VHDX header")),
        };
        if u16le(header, 66) != 1
            || (header[48..64].iter().any(|&v| v != 0) && u16le(header, 64) != 0)
        {
            return Err(unsupported("unsupported VHDX log or format version"));
        }
        if header[48..64].iter().any(|&v| v != 0) {
            return Err(unsupported("VHDX requires log replay before reading"));
        }
        let log_start = u64le(header, 72);
        let log_len = u32le(header, 68) as u64;
        if log_len < M
            || !log_len.is_multiple_of(M)
            || log_start < M
            || !log_start.is_multiple_of(M)
        {
            return Err(invalid("invalid VHDX log extent"));
        }
        check_range(log_start, log_len, source.len())?;
        let mut protected = vec![(0, M), (log_start, log_start + log_len)];
        let t1 = read(source.as_ref(), &budget, 196608, 65536)?;
        let t2 = read(source.as_ref(), &budget, 262144, 65536)?;
        let table = match (region_table(&t1), region_table(&t2)) {
            (true, true) => {
                let n = u32le(&t1, 8) as usize;
                if n != u32le(&t2, 8) as usize || t1[16..16 + n * 32] != t2[16..16 + n * 32] {
                    return Err(invalid("inconsistent VHDX region tables"));
                }
                &t1
            }
            (true, false) => &t1,
            (false, true) => &t2,
            _ => return Err(invalid("no valid VHDX region table")),
        };
        let mut bat = None;
        let mut meta = None;
        let mut ids = BTreeSet::new();
        for e in table[16..16 + u32le(table, 8) as usize * 32]
            .as_chunks::<32>()
            .0
        {
            budget.work(1)?;
            let guid: [u8; 16] = e[..16].try_into().unwrap();
            if !ids.insert(guid) {
                return Err(invalid("duplicate VHDX region"));
            }
            let start = u64le(e, 16);
            let len = u32le(e, 24) as u64;
            let required = u32le(e, 28);
            if required > 1
                || start < M
                || !start.is_multiple_of(M)
                || len == 0
                || !len.is_multiple_of(M)
            {
                return Err(invalid("invalid VHDX region geometry"));
            }
            check_range(start, len, source.len())?;
            protected.push((start, start + len));
            if guid == BAT {
                bat = Some((start, len));
            } else if guid == META {
                meta = Some((start, len));
            } else if required == 1 {
                return Err(unsupported("unknown required VHDX region"));
            }
        }
        protected.sort_unstable();
        if protected.windows(2).any(|w| overlaps(w[0], w[1])) {
            return Err(invalid("overlapping VHDX structures"));
        }
        let bat = bat.ok_or_else(|| invalid("missing VHDX BAT region"))?;
        let meta = meta.ok_or_else(|| invalid("missing VHDX metadata region"))?;
        let metadata = read(source.as_ref(), &budget, meta.0, 65536)?;
        if &metadata[..8] != b"metadata"
            || u16le(&metadata, 8) != 0
            || metadata[12..32].iter().any(|&v| v != 0)
            || u16le(&metadata, 10) > 2047
        {
            return Err(invalid("invalid VHDX metadata table"));
        }
        let mut known = [const { None }; 5];
        let mut size_offset = 0;
        let mut locator = None;
        let guids = [PARAM, SIZE, ID, LOGICAL, PHYSICAL];
        let sizes = [8, 8, 16, 4, 4];
        let mut seen = BTreeSet::new();
        let mut extents = Vec::new();
        for e in metadata[32..32 + u16le(&metadata, 10) as usize * 32]
            .as_chunks::<32>()
            .0
        {
            budget.work(1)?;
            let guid: [u8; 16] = e[..16].try_into().unwrap();
            let flags = u32le(e, 24);
            let start = u32le(e, 16) as u64;
            let len = u32le(e, 20) as u64;
            if flags & !7 != 0 || u32le(e, 28) != 0 || !seen.insert((guid, flags & 1)) {
                return Err(invalid("invalid or duplicate VHDX metadata entry"));
            }
            if len == 0 {
                if start != 0 {
                    return Err(invalid("invalid empty VHDX metadata"));
                }
            } else {
                if start < 65536 || len > M {
                    return Err(invalid("invalid VHDX metadata extent"));
                }
                check_range(start, len, meta.1)?;
                extents.push((start, start + len));
            }
            if let Some(index) = guids.iter().position(|g| *g == guid) {
                if flags != if index == 0 { 4 } else { 6 } || len != sizes[index] {
                    return Err(invalid("invalid required VHDX metadata"));
                }
                if index == 1 {
                    size_offset = meta.0 + start;
                }
                known[index] = Some(read(
                    source.as_ref(),
                    &budget,
                    meta.0 + start,
                    len as usize,
                )?);
            } else if guid == parent::ITEM {
                if flags != 4 || len < 20 {
                    return Err(invalid("invalid required VHDX parent locator"));
                }
                budget.work(len * 20)?;
                let _locator_cache = budget.cache(len * 8)?;
                locator = Some(parent::parse(&read(
                    source.as_ref(),
                    &budget,
                    meta.0 + start,
                    len as usize,
                )?)?);
            } else if flags & 4 != 0 {
                return Err(unsupported("unknown required VHDX metadata"));
            }
        }
        extents.sort_unstable();
        if extents.windows(2).any(|w| overlaps(w[0], w[1])) {
            return Err(invalid("overlapping VHDX metadata"));
        }
        if known.iter().any(Option::is_none) {
            return Err(invalid("missing required VHDX metadata"));
        }
        let params = known[0].as_ref().unwrap();
        let block = u32le(params, 0) as u64;
        let flags = u32le(params, 4);
        let differencing = flags & 2 != 0;
        if differencing != locator.is_some() {
            return Err(invalid("inconsistent VHDX parent metadata"));
        }
        if differencing && !allow_parent {
            return Err(unsupported(
                "VHDX differencing images require an authorized parent",
            ));
        }
        if flags & !3 != 0 {
            return Err(unsupported("unknown VHDX file parameter flags"));
        }
        let length = u64le(known[1].as_ref().unwrap(), 0);
        let sector = u32le(known[3].as_ref().unwrap(), 0) as u64;
        let physical = u32le(known[4].as_ref().unwrap(), 0);
        if !(M..=256 * M).contains(&block)
            || !block.is_power_of_two()
            || ![512, 4096].contains(&sector)
            || ![512, 4096].contains(&physical)
            || length == 0
            || length > 64 * (1 << 40)
            || !length.is_multiple_of(sector)
        {
            return Err(invalid("invalid VHDX disk geometry"));
        }
        let count = length.div_ceil(block);
        let ratio = (1u64 << 23) * sector / block;
        let chunks = count.div_ceil(ratio);
        let entries = if differencing {
            chunks * (ratio + 1)
        } else {
            count + (count - 1) / ratio
        };
        let bytes = entries * 8;
        check_range(0, bytes, bat.1)?;
        budget.work(entries)?;
        let cache = budget.cache(count * 9 + chunks * 8 + if differencing { M * 8 } else { 0 })?;
        let _bat_cache = budget.cache(bytes)?;
        let _ownership = budget.cache((count + chunks) * 16)?;
        budget.metadata(count * 24)?;
        let encoded = read(
            source.as_ref(),
            &budget,
            bat.0,
            usize::try_from(bytes).map_err(|_| invalid("VHDX BAT too large"))?,
        )?;
        let mut map = Vec::with_capacity(count as usize);
        let mut allocated = Vec::new();
        let mut states = Vec::with_capacity(count as usize);
        let mut bitmaps = vec![0; chunks as usize];
        for (index, e) in encoded.as_chunks::<8>().0.iter().enumerate() {
            let entry = u64::from_le_bytes(*e);
            if entry & 0xffff8 != 0 {
                return Err(invalid("VHDX BAT reserved bits are nonzero"));
            }
            let state = entry & 7;
            let offset = entry & !0xfffff;
            let bitmap = (index as u64 + 1).is_multiple_of(ratio + 1);
            if bitmap {
                if (!differencing && (state != 0 || offset != 0))
                    || ![0, 6].contains(&state)
                    || (state == 6 && offset == 0)
                {
                    return Err(invalid("invalid VHDX sector bitmap allocation"));
                }
                if offset != 0 {
                    check_range(offset, M, source.len())?;
                    let extent = (offset, offset + M);
                    if protected.iter().any(|&p| overlaps(p, extent)) {
                        return Err(invalid("VHDX bitmap overlaps metadata"));
                    }
                    allocated.push(extent);
                }
                if state == 6 {
                    bitmaps[index / (ratio + 1) as usize] = offset;
                }
                continue;
            }
            let logical_index = index as u64 - index as u64 / (ratio + 1);
            if logical_index >= count {
                if entry != 0 {
                    return Err(invalid("nonzero VHDX final BAT padding"));
                }
                continue;
            }
            if state == 7 && !differencing {
                return Err(unsupported("partially present standalone VHDX"));
            }
            if state == 4 || state == 5 {
                return Err(invalid("reserved VHDX payload state"));
            }
            if offset != 0 {
                check_range(offset, block, source.len())?;
                let extent = (offset, offset + block);
                budget.work(protected.len() as u64)?;
                if protected.iter().any(|&p| overlaps(p, extent)) {
                    return Err(invalid("VHDX payload overlaps metadata"));
                }
                allocated.push(extent);
            }
            if (state == 6 || state == 7) && offset == 0 {
                return Err(invalid("present VHDX payload has no allocation"));
            }
            map.push(if state == 6 || state == 7 { offset } else { 0 });
            states.push(state as u8);
        }
        for (index, &state) in states.iter().enumerate() {
            if state == 7 && bitmaps[index / ratio as usize] == 0 {
                return Err(invalid("partial VHDX payload missing sector bitmap"));
            }
        }
        budget.work((allocated.len() as u64).saturating_mul(count.ilog2() as u64 + 1))?;
        allocated.sort_unstable();
        if allocated.windows(2).any(|w| overlaps(w[0], w[1])) {
            return Err(invalid("multiply owned VHDX payload allocation"));
        }
        Ok(Self {
            source,
            length,
            block,
            bat_offset: bat.0,
            logical_sector: sector as u32,
            physical_sector: physical,
            map,
            states,
            bitmaps,
            parent: None,
            locator,
            data_guid: header[32..48].try_into().unwrap(),
            metadata_offset: meta.0,
            bat_length: bat.1,
            size_offset,
            leave_blocks_allocated: flags & 1 != 0,
            budget,
            _cache: cache,
        })
    }
}
impl ReadAt for Vhdx {
    fn visit_extents(
        &self,
        visitor: &mut dyn FnMut(crate::DiskExtent) -> io::Result<()>,
    ) -> io::Result<()> {
        let result = (|| {
            let mut pending: Option<crate::DiskExtent> = None;
            let mut offset = 0;
            while offset < self.length {
                self.budget.work(1)?;
                let index = (offset / self.block) as usize;
                let state = self.states[index];
                let mut length = (self.length - offset).min(self.block - offset % self.block);
                let kind = if state == 7 {
                    length = length.min(self.logical_sector as u64);
                    let chunk = (1u64 << 23) * self.logical_sector as u64;
                    let bit = (offset % chunk) / self.logical_sector as u64;
                    let mut byte = [0];
                    self.budget.metadata(1)?;
                    self.source.read_exact_at(
                        self.bitmaps[(offset / chunk) as usize] + bit / 8,
                        &mut byte,
                    )?;
                    if byte[0] & (1 << (bit % 8)) != 0 {
                        crate::ExtentKind::Allocated
                    } else {
                        crate::ExtentKind::Inherited
                    }
                } else if state == 6 {
                    crate::ExtentKind::Allocated
                } else if state == 0 && self.parent.is_some() {
                    crate::ExtentKind::Inherited
                } else {
                    crate::ExtentKind::Zero
                };
                let start = offset;
                offset += length;
                if let Some(run) = &mut pending {
                    if run.kind == kind {
                        run.length += length;
                        continue;
                    }
                    visitor(*run)?;
                }
                pending = Some(crate::DiskExtent {
                    offset: start,
                    length,
                    kind,
                });
            }
            if let Some(run) = pending {
                visitor(run)?;
            }
            Ok(())
        })();
        result.map_err(|e| self.context().error("map VHDX", e))
    }
    fn len(&self) -> u64 {
        self.length
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
            let mut at = offset;
            let mut remaining = destination;
            while !remaining.is_empty() {
                self.budget.work(1)?;
                let within = at % self.block;
                let index = (at / self.block) as usize;
                let state = self.states[index];
                let n = if state == 7 {
                    (self.logical_sector as u64 - at % self.logical_sector as u64)
                        .min(remaining.len() as u64) as usize
                } else {
                    (self.block - within).min(remaining.len() as u64) as usize
                };
                let (out, next) = remaining.split_at_mut(n);
                let physical = self.map[index];
                let private = if state == 7 {
                    let chunk = (1u64 << 23) * self.logical_sector as u64;
                    let bit = (at % chunk) / self.logical_sector as u64;
                    let mut byte = [0];
                    self.budget.metadata(1)?;
                    self.source
                        .read_exact_at(self.bitmaps[(at / chunk) as usize] + bit / 8, &mut byte)?;
                    byte[0] & (1 << (bit % 8)) != 0
                } else {
                    state == 6
                };
                if private {
                    self.source.read_exact_at(physical + within, out)?;
                } else if state == 0 || state == 7 {
                    if let Some(parent) = &self.parent {
                        parent.read_exact_at(at, out)?;
                    } else {
                        out.fill(0);
                    }
                } else {
                    out.fill(0);
                }
                at += n as u64;
                remaining = next;
            }
            Ok(())
        })();
        result.map_err(|e| {
            let mut context = self.context();
            context.offset = Some(offset);
            context.error("read VHDX", e)
        })
    }
}
