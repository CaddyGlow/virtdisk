//! Read-only MS-VHDX log replay into an immutable overlay.
use super::{M, checksum, invalid, read, u16le, u32le, u64le, unsupported};
use crate::{CacheReservation, ReadAt, ReadBudget, ReadContext, check_range};
use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    sync::Arc,
};
#[derive(Clone, Copy)]
struct Entry {
    start: usize,
    length: usize,
    tail: usize,
    sequence: u64,
    count: usize,
    last: u64,
    flushed: u64,
}
struct Patch {
    start: u64,
    length: u64,
    data: Option<Box<[u8; 4096]>>,
}
pub(crate) struct Overlay {
    source: Arc<dyn ReadAt>,
    length: u64,
    patches: Vec<Patch>,
    budget: ReadBudget,
    _cache: Vec<CacheReservation>,
    log_guid: Option<[u8; 16]>,
}
impl Overlay {
    pub(crate) fn reserve_replay_work(&self) -> io::Result<()> {
        if self.log_guid.is_none() {
            return Ok(());
        }
        let mut work = 0u64;
        for patch in &self.patches[..self.patches.len() - 1] {
            work = work
                .checked_add(1)
                .and_then(|n| {
                    n.checked_add(if patch.data.is_none() {
                        patch.length.div_ceil(65536)
                    } else {
                        0
                    })
                })
                .ok_or_else(|| unsupported("VHDX native replay work overflows"))?;
        }
        self.budget.work(work)
    }
    pub(crate) fn native_header(&self) -> Option<(u64, [u8; 4096])> {
        let guid = self.log_guid?;
        let patch = self.patches.last()?;
        let mut header = **patch.data.as_ref()?;
        header[48..64].copy_from_slice(&guid);
        Some((patch.start, header))
    }
    pub(crate) fn replay_to(
        &self,
        file: &mut File,
        mut applied: impl FnMut(usize) -> io::Result<()>,
    ) -> io::Result<()> {
        file.set_len(self.length)?;
        let zero = [0; 65536];
        for (index, patch) in self.patches[..self.patches.len() - 1].iter().enumerate() {
            file.seek(SeekFrom::Start(patch.start))?;
            if let Some(data) = &patch.data {
                file.write_all(data.as_ref())?;
            } else {
                let mut remaining = patch.length;
                while remaining != 0 {
                    let length = remaining.min(zero.len() as u64) as usize;
                    file.write_all(&zero[..length])?;
                    remaining -= length as u64;
                }
            }
            applied(index)?;
        }
        Ok(())
    }
    pub(super) fn reject_payload_updates(&self, payload: &[u64], block: u64) -> io::Result<()> {
        for &start in payload.iter().filter(|&&v| v != 0) {
            self.budget.work(self.patches.len() as u64)?;
            if self
                .patches
                .iter()
                .any(|p| p.start < start + block && start < p.start + p.length)
            {
                return Err(invalid("VHDX log attempts to update payload data"));
            }
        }
        Ok(())
    }
}
impl ReadAt for Overlay {
    fn len(&self) -> u64 {
        self.length
    }
    fn context(&self) -> ReadContext {
        self.source.context()
    }
    fn budget(&self) -> Option<ReadBudget> {
        Some(self.budget.clone())
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        check_range(offset, out.len() as u64, self.length)?;
        let base_len = self
            .source
            .len()
            .saturating_sub(offset)
            .min(out.len() as u64) as usize;
        out.fill(0);
        if base_len != 0 {
            self.source.read_exact_at(offset, &mut out[..base_len])?;
        }
        let end = offset + out.len() as u64;
        self.budget.work(self.patches.len() as u64)?;
        for patch in &self.patches {
            let first = offset.max(patch.start);
            let last = end.min(patch.start + patch.length);
            if first < last {
                let target = &mut out[(first - offset) as usize..(last - offset) as usize];
                if let Some(data) = &patch.data {
                    target.copy_from_slice(
                        &data[(first - patch.start) as usize..(last - patch.start) as usize],
                    );
                } else {
                    target.fill(0);
                }
            }
        }
        Ok(())
    }
}
fn crc_ring(log: &[u8], start: usize, length: usize) -> u32 {
    let mut c = !0u32;
    for i in 0..length {
        let b = if (4..8).contains(&i) {
            0
        } else {
            log[(start + i) % log.len()]
        };
        c ^= b as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    !c
}
fn descriptor<'a>(log: &'a [u8], entry: &Entry, index: usize) -> &'a [u8] {
    let start = (entry.start + 64 + index * 32) % log.len();
    &log[start..start + 32]
}
fn data_sector<'a>(log: &'a [u8], entry: &Entry, index: usize) -> &'a [u8] {
    let descriptors = (64 + entry.count * 32).div_ceil(4096) * 4096;
    let start = (entry.start + descriptors + index * 4096) % log.len();
    &log[start..start + 4096]
}
fn parse(
    log: &[u8],
    start: usize,
    guid: &[u8],
    budget: &ReadBudget,
    log_start: u64,
) -> io::Result<Option<Entry>> {
    let h = &log[start..start + 64];
    if &h[..4] != b"loge" {
        return Ok(None);
    }
    let length = u32le(h, 8) as usize;
    let tail = u32le(h, 12) as usize;
    let count = u32le(h, 24) as usize;
    let sequence = u64le(h, 16);
    let last = u64le(h, 56);
    let flushed = u64le(h, 48);
    let desc_bytes = 64u64 + count as u64 * 32;
    let desc_length = desc_bytes.div_ceil(4096) * 4096;
    if length < 4096
        || length > log.len()
        || !length.is_multiple_of(4096)
        || tail >= log.len()
        || !tail.is_multiple_of(4096)
        || sequence == 0
        || u32le(h, 28) != 0
        || &h[32..48] != guid
        || !last.is_multiple_of(M)
        || !flushed.is_multiple_of(M)
        || desc_length > length as u64
    {
        return Ok(None);
    }
    budget.work(length as u64)?;
    if crc_ring(log, start, length) != u32le(h, 4) {
        return Ok(None);
    }
    let entry = Entry {
        start,
        length,
        tail,
        sequence,
        count,
        last,
        flushed,
    };
    let mut data_count = 0usize;
    for index in 0..count {
        budget.work(1)?;
        let d = descriptor(log, &entry, index);
        let offset = u64le(d, 16);
        if !offset.is_multiple_of(4096) || u64le(d, 24) != sequence {
            return Ok(None);
        }
        let len = if &d[..4] == b"zero" {
            let len = u64le(d, 8);
            if u32le(d, 4) != 0 || !len.is_multiple_of(4096) {
                return Ok(None);
            }
            len
        } else if &d[..4] == b"desc" {
            data_count += 1;
            if desc_length + data_count as u64 * 4096 > length as u64 {
                return Ok(None);
            }
            let data = data_sector(log, &entry, data_count - 1);
            if &data[..4] != b"data"
                || ((u32le(data, 4) as u64) << 32 | u32le(data, 4092) as u64) != sequence
            {
                return Ok(None);
            }
            4096
        } else {
            return Ok(None);
        };
        let Some(end) = offset.checked_add(len) else {
            return Ok(None);
        };
        if end > last
            || offset < 196608
            || (offset < log_start + log.len() as u64 && log_start < end)
        {
            return Ok(None);
        }
    }
    if desc_length + data_count as u64 * 4096 != length as u64 {
        return Ok(None);
    }
    Ok(Some(entry))
}
pub(crate) fn recover(source: Arc<dyn ReadAt>, budget: ReadBudget) -> io::Result<Arc<Overlay>> {
    let source = budget.reader(source);
    let _headers = budget.cache(8192)?;
    if read(source.as_ref(), &budget, 0, 8)? != b"vhdxfile" {
        return Err(invalid("invalid VHDX identifier"));
    }
    let h1 = read(source.as_ref(), &budget, 65536, 4096)?;
    let h2 = read(source.as_ref(), &budget, 131072, 4096)?;
    let valid = |h: &[u8]| &h[..4] == b"head" && checksum(h);
    let (offset, header) = match (valid(&h1), valid(&h2)) {
        (true, true) => {
            let a = u64le(&h1, 8);
            let b = u64le(&h2, 8);
            if a == b {
                return Err(invalid("ambiguous VHDX header sequences"));
            }
            if a > b { (65536, h1) } else { (131072, h2) }
        }
        (true, false) => (65536, h1),
        (false, true) => (131072, h2),
        _ => return Err(invalid("no valid VHDX recovery header")),
    };
    if u16le(&header, 66) != 1 {
        return Err(unsupported("unsupported VHDX version"));
    }
    let guid = &header[48..64];
    if guid.iter().all(|&v| v == 0) {
        return Ok(Arc::new(Overlay {
            length: source.len(),
            source,
            patches: Vec::new(),
            budget,
            _cache: Vec::new(),
            log_guid: None,
        }));
    }
    if u16le(&header, 64) != 0 {
        return Err(unsupported("unsupported VHDX log version"));
    }
    let log_start = u64le(&header, 72);
    let log_length = u32le(&header, 68) as u64;
    if log_start < M
        || !log_start.is_multiple_of(M)
        || log_length < M
        || !log_length.is_multiple_of(M)
    {
        return Err(invalid("invalid VHDX log extent"));
    }
    check_range(log_start, log_length, source.len())?;
    let _log_cache = budget.cache(log_length)?;
    let log = read(source.as_ref(), &budget, log_start, log_length as usize)?;
    let sectors = log.len() / 4096;
    let _entry_cache = budget.cache(sectors as u64 * 64)?;
    budget.metadata(sectors as u64 * 64)?;
    let mut entries = vec![None; sectors];
    for (index, slot) in entries.iter_mut().enumerate() {
        budget.work(1)?;
        *slot = parse(&log, index * 4096, guid, &budget, log_start)?;
    }
    let _selection = budget.cache(sectors as u64 * 24)?;
    let mut active = Vec::new();
    let mut newest = 0u64;
    for start in 0..sectors {
        let mut path = Vec::new();
        let mut head = start;
        let mut previous = None;
        let mut consumed = 0usize;
        loop {
            budget.work(1)?;
            let Some(entry) = entries[head] else {
                break;
            };
            if previous.is_some_and(|n: u64| n.checked_add(1) != Some(entry.sequence))
                || consumed + entry.length > log.len()
            {
                break;
            }
            budget.metadata(8)?;
            path.push(head);
            consumed += entry.length;
            previous = Some(entry.sequence);
            // A complete sequence's newest entry must point at a contained tail.
            if entry.sequence > newest {
                budget.work(path.len() as u64)?;
                if let Some(tail) = path.iter().position(|&index| index * 4096 == entry.tail) {
                    budget.metadata((path.len() - tail) as u64 * 8)?;
                    active = path[tail..].to_vec();
                    newest = entry.sequence;
                }
            }
            head = ((entry.start + entry.length) % log.len()) / 4096;
            if consumed == log.len() {
                break;
            }
        }
    }
    if active.is_empty() {
        return Err(invalid("VHDX log has no complete valid active sequence"));
    }
    let newest = entries[*active.last().unwrap()].unwrap();
    if source.len() < newest.flushed {
        return Err(invalid("VHDX image truncated below flushed log boundary"));
    }
    let mut patches = Vec::new();
    let mut reservations = Vec::new();
    let mut length = source.len().max(newest.last);
    for index in active {
        let entry = entries[index].unwrap();
        let mut data_index = 0usize;
        for index in 0..entry.count {
            budget.work(1)?;
            let d = descriptor(&log, &entry, index);
            let start = u64le(d, 16);
            reservations.push(budget.cache(64)?);
            budget.metadata(64)?;
            let patch = if &d[..4] == b"zero" {
                Patch {
                    start,
                    length: u64le(d, 8),
                    data: None,
                }
            } else {
                reservations.push(budget.cache(4096)?);
                budget.metadata(4096)?;
                let mut bytes = Box::new([0; 4096]);
                bytes.copy_from_slice(data_sector(&log, &entry, data_index));
                data_index += 1;
                bytes[..8].copy_from_slice(&d[8..16]);
                bytes[4092..].copy_from_slice(&d[4..8]);
                Patch {
                    start,
                    length: 4096,
                    data: Some(bytes),
                }
            };
            length = length.max(start + patch.length);
            patches.push(patch);
        }
    }
    // Clear only the selected header's log GUID in the immutable recovered view.
    reservations.push(budget.cache(4096 + 64)?);
    budget.metadata(4096 + 64)?;
    let mut clean = Box::new([0; 4096]);
    clean.copy_from_slice(&header);
    clean[48..64].fill(0);
    clean[4..8].fill(0);
    let mut c = !0u32;
    for &v in clean.iter() {
        c ^= v as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    clean[4..8].copy_from_slice(&(!c).to_le_bytes());
    patches.push(Patch {
        start: offset,
        length: 4096,
        data: Some(clean),
    });
    Ok(Arc::new(Overlay {
        source,
        length,
        patches,
        budget,
        _cache: reservations,
        log_guid: Some(guid.try_into().unwrap()),
    }))
}
