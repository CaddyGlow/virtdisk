//! One-block VHDX allocation through retained native redo records.
use super::{State, VhdxWriter};
use crate::ReadAt;
use std::io::{self, Read, Seek, SeekFrom, Write};
const M: u64 = 1 << 20;
#[derive(Clone, Copy)]
pub(super) struct LogEpoch {
    guid: [u8; 16],
    offset: u64,
    sequence: u64,
    slot: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    CowChunkWritten(u64),
    PayloadSynced,
    StagedLogSynced,
    ActivatedFirstSynced,
    ActivatedSecondSynced,
    RedoLogSynced,
    BatPublished,
    MetadataWritten(u64),
    ClearFirstSynced,
    ClearSecondSynced,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn put64(b: &mut [u8], at: usize, n: u64) {
    b[at..at + 8].copy_from_slice(&n.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, n: u32) {
    b[at..at + 4].copy_from_slice(&n.to_le_bytes());
}
fn headers(
    state: &mut State,
    mut header: [u8; 4096],
    first: Stage,
    second: Stage,
    hook: &mut impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    let sequence = u64::from_le_bytes(state.header[8..16].try_into().unwrap());
    let end = sequence
        .checked_add(2)
        .ok_or_else(|| invalid("VHDX transaction header sequence exhausted"))?;
    let inactive = if state.active == 65536 { 131072 } else { 65536 };
    for (offset, sequence, stage) in [(inactive, sequence + 1, first), (state.active, end, second)]
    {
        put64(&mut header, 8, sequence);
        crate::vhdx_write::checksum(&mut header);
        state.file.seek(SeekFrom::Start(offset))?;
        state.file.write_all(&header)?;
        state.file.sync_all()?;
        hook(stage)?;
    }
    state.header = header;
    Ok(())
}
enum Update {
    Data { offset: u64, body: Box<[u8; 4096]> },
    Zero { offset: u64, length: u64 },
}
impl Update {
    fn range(&self) -> (u64, u64) {
        match self {
            Self::Data { offset, .. } => (*offset, 4096),
            Self::Zero { offset, length } => (*offset, *length),
        }
    }
}
fn log_record(log: LogEpoch, updates: &[Update], length: u64) -> io::Result<Vec<u8>> {
    let data_count = updates
        .iter()
        .filter(|u| matches!(u, Update::Data { .. }))
        .count();
    if updates.len() > 4 || data_count > 3 {
        return Err(invalid("VHDX bounded log descriptor count exceeded"));
    }
    let size = (1 + data_count) * 4096;
    let mut entry = vec![0; size];
    entry[..4].copy_from_slice(b"loge");
    put32(&mut entry, 8, size as u32);
    put32(&mut entry, 12, log.slot as u32);
    put64(&mut entry, 16, log.sequence);
    put32(&mut entry, 24, updates.len() as u32);
    entry[32..48].copy_from_slice(&log.guid);
    put64(&mut entry, 48, length);
    put64(&mut entry, 56, length);
    let mut ranges = Vec::new();
    let mut data = 0;
    for (index, update) in updates.iter().enumerate() {
        let (offset, n) = update.range();
        let end = offset
            .checked_add(n)
            .ok_or_else(|| invalid("VHDX logged extent overflow"))?;
        if !offset.is_multiple_of(4096)
            || n == 0
            || !n.is_multiple_of(4096)
            || offset < 196608
            || end > length
            || (offset < log.offset + M && log.offset < end)
            || ranges
                .iter()
                .any(|&(start, end_old)| offset < end_old && start < end)
        {
            return Err(invalid("invalid or overlapping VHDX logged target"));
        }
        ranges.push((offset, end));
        let at = 64 + index * 32;
        put64(&mut entry, at + 16, offset);
        put64(&mut entry, at + 24, log.sequence);
        match update {
            Update::Zero { length, .. } => {
                entry[at..at + 4].copy_from_slice(b"zero");
                put64(&mut entry, at + 8, *length);
            }
            Update::Data { body, .. } => {
                entry[at..at + 4].copy_from_slice(b"desc");
                entry[at + 4..at + 8].copy_from_slice(&body[4092..]);
                entry[at + 8..at + 16].copy_from_slice(&body[..8]);
                let slot = 4096 + data * 4096;
                entry[slot..slot + 4].copy_from_slice(b"data");
                put32(&mut entry, slot + 4, (log.sequence >> 32) as u32);
                entry[slot + 8..slot + 4092].copy_from_slice(&body[8..4092]);
                put32(&mut entry, slot + 4092, log.sequence as u32);
                data += 1;
            }
        }
    }
    crate::vhdx_write::checksum(&mut entry);
    Ok(entry)
}
pub(super) fn allocate(
    writer: &VhdxWriter,
    state: &mut State,
    index: usize,
    within: u64,
    bytes: &[u8],
    hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    mutate(writer, state, index, Some((within, bytes)), hook)
}

pub(super) fn discard(
    writer: &VhdxWriter,
    state: &mut State,
    index: usize,
    hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    mutate(writer, state, index, None, hook)
}

fn mutate(
    writer: &VhdxWriter,
    state: &mut State,
    index: usize,
    mutation: Option<(u64, &[u8])>,
    mut hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    let result = (|| {
        if state.failed {
            return Err(io::Error::other(
                "VHDX metadata transaction failed; recover and reopen",
            ));
        }
        let header_sequence = u64::from_le_bytes(state.header[8..16].try_into().unwrap());
        header_sequence
            .checked_add(if state.epoch { 4 } else { 6 })
            .ok_or_else(|| invalid("VHDX transaction header sequence exhausted"))?;
        let _scratch = writer.budget.cache(16384)?;
        writer.budget.metadata(16384)?;
        writer.budget.work(20)?;
        let ratio = (1u64 << 23) * writer.logical_sector as u64 / writer.block;
        let index = index as u64;
        let bat_entry = writer
            .bat_offset
            .checked_add((index + index / ratio) * 8)
            .ok_or_else(|| invalid("VHDX BAT offset overflow"))?;
        let sector_offset = bat_entry / 4096 * 4096;
        let mut sector = [0; 4096];
        state.file.seek(SeekFrom::Start(sector_offset))?;
        state.file.read_exact(&mut sector)?;
        let old_length = state.file.metadata()?.len();
        let payload = old_length
            .checked_add(M - 1)
            .map(|n| n / M * M)
            .ok_or_else(|| invalid("VHDX allocation offset overflow"))?;
        let payload_end = if mutation.is_some() {
            payload
                .checked_add(writer.block)
                .ok_or_else(|| invalid("VHDX payload extent overflow"))?
        } else {
            payload
        };
        let fresh = state.log_epoch.is_none();
        let log = if let Some(log) = state.log_epoch {
            log
        } else {
            LogEpoch {
                guid: crate::vhdx_write::identity()?,
                offset: payload_end,
                sequence: 1,
                slot: 0,
            }
        };
        let _next_sequence = log
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("VHDX native log sequence exhausted"))?;
        let final_length = if fresh {
            payload_end
                .checked_add(M)
                .ok_or_else(|| invalid("VHDX log extent overflow"))?
        } else {
            if mutation.is_some() {
                payload_end
            } else {
                old_length
            }
        };
        put64(
            &mut sector,
            (bat_entry - sector_offset) as usize,
            if mutation.is_some() { payload | 6 } else { 2 },
        );
        let copy = mutation.is_some()
            && writer.requires_copy(
                index as usize,
                state.map[index as usize],
                state.zero_masks[index as usize],
            );
        let _copy_cache = if copy {
            writer.budget.work(writer.block.div_ceil(65536))?;
            Some(writer.budget.cache(65536)?)
        } else {
            None
        };
        VhdxWriter::epoch(state)?;
        // Newly extended payload bytes are zero; the requested slice is persisted
        // before metadata can make this complete block reachable.
        state.file.set_len(final_length)?;
        if copy {
            let base = writer.base.as_ref().unwrap();
            let mut buffer = [0; 65536];
            let logical = index * writer.block;
            let amount = (writer.length - logical).min(writer.block);
            let mut copied = 0;
            while copied < amount {
                let n = (amount - copied).min(buffer.len() as u64) as usize;
                base.read_exact_at(logical + copied, &mut buffer[..n])?;
                state.file.seek(SeekFrom::Start(payload + copied))?;
                state.file.write_all(&buffer[..n])?;
                copied += n as u64;
                hook(Stage::CowChunkWritten(copied))?;
            }
        }
        if let Some((within, bytes)) = mutation {
            state.file.seek(SeekFrom::Start(payload + within))?;
            state.file.write_all(bytes)?;
        }
        state.file.sync_all()?;
        hook(Stage::PayloadSynced)?;
        publish_record(state, log, final_length, sector_offset, &sector, &mut hook)?;
        state.map[index as usize] = if mutation.is_some() { payload } else { 0 };
        state.zero_masks[index as usize] = mutation.is_none();
        state.states[index as usize] = if mutation.is_some() { 6 } else { 2 };
        Ok(())
    })();
    if result.is_err() {
        state.failed = true;
    }
    result
}

fn publish_record(
    state: &mut State,
    log: LogEpoch,
    final_length: u64,
    target: u64,
    sector: &[u8; 4096],
    hook: &mut impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    publish_updates(
        state,
        log,
        final_length,
        &[Update::Data {
            offset: target,
            body: Box::new(*sector),
        }],
        hook,
    )
}
fn publish_updates(
    state: &mut State,
    log: LogEpoch,
    final_length: u64,
    updates: &[Update],
    hook: &mut impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    let fresh = state.log_epoch.is_none();
    let next_sequence = log
        .sequence
        .checked_add(1)
        .ok_or_else(|| invalid("VHDX log sequence exhausted"))?;
    let entry = log_record(log, updates, final_length)?;
    if fresh {
        state.file.seek(SeekFrom::Start(log.offset + log.slot))?;
        state.file.write_all(&entry)?;
        state.file.sync_all()?;
        hook(Stage::StagedLogSynced)?;
    }
    let mut header = state.header;
    header[48..64].copy_from_slice(&log.guid);
    header[64..66].fill(0);
    put32(&mut header, 68, M as u32);
    put64(&mut header, 72, log.offset);
    headers(
        state,
        header,
        Stage::ActivatedFirstSynced,
        Stage::ActivatedSecondSynced,
        hook,
    )?;
    if !fresh {
        state.file.seek(SeekFrom::Start(log.offset + log.slot))?;
        state.file.write_all(&entry)?;
        state.file.sync_all()?;
    }
    hook(Stage::RedoLogSynced)?;
    for (index, update) in updates.iter().enumerate() {
        let (offset, _) = update.range();
        state.file.seek(SeekFrom::Start(offset))?;
        match update {
            Update::Data { body, .. } => state.file.write_all(body.as_ref())?,
            Update::Zero { length, .. } => {
                let zero = [0; 65536];
                let mut left = *length;
                while left != 0 {
                    let n = left.min(zero.len() as u64) as usize;
                    state.file.write_all(&zero[..n])?;
                    left -= n as u64;
                }
            }
        }
        hook(Stage::MetadataWritten(index as u64))?;
    }
    state.file.sync_all()?;
    hook(Stage::BatPublished)?;
    let mut clean = state.header;
    clean[48..64].fill(0);
    headers(
        state,
        clean,
        Stage::ClearFirstSynced,
        Stage::ClearSecondSynced,
        hook,
    )?;
    state.log_epoch = Some(LogEpoch {
        guid: log.guid,
        offset: log.offset,
        sequence: next_sequence,
        slot: if log.slot == 0 { 16384 } else { 0 },
    });
    Ok(())
}

struct UpdatesView {
    source: std::sync::Arc<dyn ReadAt>,
    updates: Vec<Update>,
}
impl ReadAt for UpdatesView {
    fn len(&self) -> u64 {
        self.source.len()
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, out.len() as u64, self.len())?;
        let mut at = offset;
        let mut done = 0;
        while done < out.len() {
            let found = self.updates.iter().find(|u| {
                let (start, len) = u.range();
                at >= start && at < start + len
            });
            let n = if let Some(update) = found {
                let (start, len) = update.range();
                let n = (start + len - at).min((out.len() - done) as u64) as usize;
                match update {
                    Update::Data { body, .. } => out[done..done + n]
                        .copy_from_slice(&body[(at - start) as usize..(at - start) as usize + n]),
                    Update::Zero { .. } => out[done..done + n].fill(0),
                }
                n
            } else {
                let end = self
                    .updates
                    .iter()
                    .map(|u| u.range().0)
                    .filter(|&p| p > at)
                    .min()
                    .unwrap_or(self.len());
                let n = (end - at).min((out.len() - done) as u64) as usize;
                self.source.read_exact_at(at, &mut out[done..done + n])?;
                n
            };
            at += n as u64;
            done += n;
        }
        Ok(())
    }
}
fn bat_update(
    writer: &VhdxWriter,
    state: &mut State,
    entry: u64,
    value: u64,
) -> io::Result<Update> {
    let position = writer.bat_offset + entry * 8;
    crate::check_range(entry * 8, 8, writer.bat_length)?;
    let target = position / 4096 * 4096;
    let mut body = Box::new([0; 4096]);
    state.file.seek(SeekFrom::Start(target))?;
    state.file.read_exact(body.as_mut())?;
    put64(body.as_mut(), (position - target) as usize, value);
    Ok(Update::Data {
        offset: target,
        body,
    })
}
fn commit_partial_updates(
    writer: &VhdxWriter,
    state: &mut State,
    updates: Vec<Update>,
    payload_end: u64,
    mut hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    preflight_metadata_transactions(state, 1)?;
    let zero_bytes = updates
        .iter()
        .map(|update| match update {
            Update::Zero { length, .. } => *length,
            _ => 0,
        })
        .sum::<u64>();
    let _scratch = writer
        .budget
        .cache(32768 + if zero_bytes != 0 { 65536 } else { 0 })?;
    writer.budget.metadata(32768 + zero_bytes)?;
    writer.budget.work(32 + zero_bytes.div_ceil(65536))?;
    let fresh = state.log_epoch.is_none();
    let log = if let Some(log) = state.log_epoch {
        log
    } else {
        LogEpoch {
            guid: crate::vhdx_write::identity()?,
            offset: payload_end,
            sequence: 1,
            slot: 0,
        }
    };
    let final_length = if fresh {
        payload_end
            .checked_add(M)
            .ok_or_else(|| invalid("VHDX log extent overflow"))?
    } else {
        state.file.metadata()?.len().max(payload_end)
    };
    // Encode and reject overlap/bounds before metadata or header publication.
    log_record(log, &updates, final_length)?;
    VhdxWriter::epoch(state)?;
    state.file.set_len(final_length)?;
    state.file.sync_all()?;
    hook(Stage::PayloadSynced)?;
    let source: std::sync::Arc<dyn ReadAt> = std::sync::Arc::new(super::FileSource::new(
        state.file.try_clone()?,
        final_length,
    ));
    crate::Vhdx::validate_writable_view(
        source.clone(),
        writer.budget.clone(),
        writer.parent.clone(),
    )?;
    let view = std::sync::Arc::new(UpdatesView { source, updates });
    crate::Vhdx::validate_writable_view(
        view.clone(),
        writer.budget.clone(),
        writer.parent.clone(),
    )?;
    publish_updates(state, log, final_length, &view.updates, &mut hook)
}
pub(super) fn partial(
    writer: &VhdxWriter,
    state: &mut State,
    index: usize,
    within: u64,
    bytes: &[u8],
    mut hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    let result = (|| {
        if state.failed || writer.parent.is_none() || !matches!(state.states[index], 0 | 7) {
            return Err(invalid("invalid native partial VHDX write profile"));
        }
        let sector = u64::from(writer.logical_sector);
        let ratio = (1u64 << 23) * sector / writer.block;
        let chunk = index as u64 / ratio;
        preflight_metadata_transactions(
            state,
            if state.bitmaps[chunk as usize] == 0 {
                2
            } else {
                1
            },
        )?;
        let _scratch = writer.budget.cache(8192)?;
        writer.budget.metadata(4096)?;
        let first = within / sector;
        let last = (within + bytes.len() as u64).div_ceil(sector);
        writer.budget.work(last - first)?;
        if state.bitmaps[chunk as usize] == 0 {
            let bitmap = state
                .file
                .metadata()?
                .len()
                .checked_add(M - 1)
                .ok_or_else(|| invalid("VHDX bitmap offset overflow"))?
                / M
                * M;
            let end = bitmap
                .checked_add(M)
                .ok_or_else(|| invalid("VHDX bitmap extent overflow"))?;
            let bat = bat_update(writer, state, (chunk + 1) * (ratio + 1) - 1, bitmap | 6)?;
            commit_partial_updates(
                writer,
                state,
                vec![
                    Update::Zero {
                        offset: bitmap,
                        length: M,
                    },
                    bat,
                ],
                end,
                &mut hook,
            )?;
            state.bitmaps[chunk as usize] = bitmap;
        }
        VhdxWriter::epoch(state)?;
        let fresh_payload = state.states[index] == 0;
        let physical = if fresh_payload {
            state
                .file
                .metadata()?
                .len()
                .checked_add(M - 1)
                .ok_or_else(|| invalid("VHDX partial payload offset overflow"))?
                / M
                * M
        } else {
            state.map[index]
        };
        let payload_end = if fresh_payload {
            physical
                .checked_add(writer.block)
                .ok_or_else(|| invalid("VHDX partial payload extent overflow"))?
        } else {
            state.file.metadata()?.len()
        };
        state.file.set_len(payload_end)?;
        let bit_start = (index as u64 % ratio) * (writer.block / sector) + first;
        let bit_end = (index as u64 % ratio) * (writer.block / sector) + last;
        if (bit_start / 32768) != (bit_end - 1) / 32768 {
            return Err(invalid("partial VHDX write crosses bitmap page"));
        }
        let page = state.bitmaps[chunk as usize] + bit_start / 32768 * 4096;
        let mut bitmap = Box::new([0; 4096]);
        state.file.seek(SeekFrom::Start(page))?;
        state.file.read_exact(bitmap.as_mut())?;
        let mut buffer = [0; 4096];
        for logical_sector in first..last {
            let logical = index as u64 * writer.block + logical_sector * sector;
            let low = within.max(logical_sector * sector);
            let high = (within + bytes.len() as u64).min((logical_sector + 1) * sector);
            if high - low != sector {
                writer.read_chunks(state, logical, &mut buffer[..sector as usize])?;
            }
            let at = (low - logical_sector * sector) as usize;
            let source = (low - within) as usize;
            let n = (high - low) as usize;
            buffer[at..at + n].copy_from_slice(&bytes[source..source + n]);
            state
                .file
                .seek(SeekFrom::Start(physical + logical_sector * sector))?;
            state.file.write_all(&buffer[..sector as usize])?;
            hook(Stage::CowChunkWritten(
                (logical_sector - first + 1) * sector,
            ))?;
        }
        state.file.sync_all()?;
        for bit in bit_start..bit_end {
            let bit = bit % 32768;
            bitmap[(bit / 8) as usize] |= 1 << (bit % 8);
        }
        let mut updates = vec![Update::Data {
            offset: page,
            body: bitmap,
        }];
        if fresh_payload {
            updates.push(bat_update(
                writer,
                state,
                index as u64 + index as u64 / ratio,
                physical | 7,
            )?);
        }
        commit_partial_updates(writer, state, updates, payload_end, &mut hook)?;
        state.map[index] = physical;
        state.states[index] = 7;
        state.zero_masks[index] = false;
        Ok(())
    })();
    if result.is_err() {
        state.failed = true;
    }
    result
}

struct SectorView {
    source: std::sync::Arc<dyn ReadAt>,
    offset: u64,
    sector: [u8; 4096],
}
impl ReadAt for SectorView {
    fn len(&self) -> u64 {
        self.source.len()
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, out.len() as u64, self.len())?;
        let mut cursor = offset;
        let mut done = 0;
        while done < out.len() {
            let n = if cursor >= self.offset && cursor < self.offset + 4096 {
                let n = (self.offset + 4096 - cursor).min((out.len() - done) as u64) as usize;
                out[done..done + n].copy_from_slice(
                    &self.sector
                        [(cursor - self.offset) as usize..(cursor - self.offset) as usize + n],
                );
                n
            } else {
                let end = if cursor < self.offset {
                    self.offset
                } else {
                    self.len()
                };
                let n = (end - cursor).min((out.len() - done) as u64) as usize;
                self.source
                    .read_exact_at(cursor, &mut out[done..done + n])?;
                n
            };
            cursor += n as u64;
            done += n;
        }
        Ok(())
    }
}

pub(super) fn preflight_metadata_transactions(state: &State, count: u64) -> io::Result<()> {
    let sequence = u64::from_le_bytes(state.header[8..16].try_into().unwrap());
    let increments = count
        .checked_mul(4)
        .and_then(|n| n.checked_add(if state.epoch { 0 } else { 2 }))
        .ok_or_else(|| invalid("VHDX resize sequence count overflow"))?;
    sequence
        .checked_add(increments)
        .ok_or_else(|| invalid("VHDX resize header sequence exhausted"))?;
    if let Some(log) = state.log_epoch {
        log.sequence
            .checked_add(count)
            .ok_or_else(|| invalid("VHDX resize log sequence exhausted"))?;
    }
    Ok(())
}

pub(super) fn metadata_sector(
    writer: &VhdxWriter,
    state: &mut State,
    target: u64,
    sector: [u8; 4096],
    mut hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    let result = (|| {
        if state.failed {
            return Err(io::Error::other(
                "VHDX transaction failed; recover and reopen",
            ));
        }
        if !target.is_multiple_of(4096)
            || !(target == writer.size_offset / 4096 * 4096
                || (target >= writer.bat_offset
                    && target + 4096 <= writer.bat_offset + writer.bat_length))
        {
            return Err(invalid("VHDX resize target is not validated metadata"));
        }
        let _scratch = writer.budget.cache(16384)?;
        writer.budget.metadata(16384)?;
        writer.budget.work(20)?;
        let sequence = u64::from_le_bytes(state.header[8..16].try_into().unwrap());
        sequence
            .checked_add(if state.epoch { 4 } else { 6 })
            .ok_or_else(|| invalid("VHDX header sequence exhausted"))?;
        let old_length = state.file.metadata()?.len();
        let source: std::sync::Arc<dyn ReadAt> =
            std::sync::Arc::new(super::FileSource::new(state.file.try_clone()?, old_length));
        crate::Vhdx::open_with_budget(source.clone(), writer.budget.clone())?;
        crate::Vhdx::open_with_budget(
            std::sync::Arc::new(SectorView {
                source,
                offset: target,
                sector,
            }),
            writer.budget.clone(),
        )?;
        let fresh = state.log_epoch.is_none();
        let log = if let Some(log) = state.log_epoch {
            log
        } else {
            LogEpoch {
                guid: crate::vhdx_write::identity()?,
                offset: old_length
                    .checked_add(M - 1)
                    .ok_or_else(|| invalid("VHDX log offset overflow"))?
                    / M
                    * M,
                sequence: 1,
                slot: 0,
            }
        };
        let _next_sequence = log
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("VHDX log sequence exhausted"))?;
        let final_length = if fresh {
            log.offset
                .checked_add(M)
                .ok_or_else(|| invalid("VHDX log extent overflow"))?
        } else {
            old_length
        };
        VhdxWriter::epoch(state)?;
        state.file.set_len(final_length)?;
        state.file.sync_all()?;
        hook(Stage::PayloadSynced)?;
        publish_record(state, log, final_length, target, &sector, &mut hook)
    })();
    if result.is_err() {
        state.failed = true;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RawDisk, ReadAt, Vhdx};
    use std::sync::Arc;
    #[test]
    fn resize_fresh_reused_log_and_final_capacity_recover_at_each_boundary() {
        let _process_boundary = crate::test_sync::writer_test();
        for shrink in [false, true] {
            for reused in [false, true] {
                for ordinal in [0, 1] {
                    for stop in [
                        Stage::PayloadSynced,
                        Stage::StagedLogSynced,
                        Stage::ActivatedFirstSynced,
                        Stage::ActivatedSecondSynced,
                        Stage::RedoLogSynced,
                        Stage::BatPublished,
                        Stage::ClearFirstSynced,
                        Stage::ClearSecondSynced,
                    ] {
                        if stop == Stage::StagedLogSynced && (reused || ordinal != 0) {
                            continue;
                        }
                        let dir = tempfile::tempdir().unwrap();
                        let path = dir.path().join("disk.vhdx");
                        let old = if shrink { 2 * M } else { M };
                        let target = if shrink { M } else { 2 * M };
                        let mut writer = VhdxWriter::create(&path, old).unwrap();
                        if reused {
                            writer.write_all_at(0, &[7]).unwrap();
                        }
                        assert_eq!(
                            writer
                                .resize_with_hook(
                                    target,
                                    crate::ShrinkPolicy::AllowDataLoss,
                                    |index, stage| {
                                        if index == ordinal && stage == stop {
                                            Err(io::Error::new(
                                                io::ErrorKind::Interrupted,
                                                "injected VHDX resize cut",
                                            ))
                                        } else {
                                            Ok(())
                                        }
                                    }
                                )
                                .unwrap_err()
                                .kind(),
                            io::ErrorKind::Interrupted
                        );
                        assert!(writer.flush().is_err());
                        drop(writer);
                        let before = std::fs::read(&path).unwrap();
                        let view =
                            Vhdx::open_recovered(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
                        let expected = if ordinal == 1
                            && matches!(
                                stop,
                                Stage::RedoLogSynced
                                    | Stage::BatPublished
                                    | Stage::ClearFirstSynced
                                    | Stage::ClearSecondSynced
                            ) {
                            target
                        } else {
                            old
                        };
                        assert_eq!(
                            view.len(),
                            expected,
                            "shrink={shrink} reused={reused} ordinal={ordinal} cut={stop:?}"
                        );
                        let mut out = [0];
                        view.read_exact_at(0, &mut out).unwrap();
                        assert_eq!(out, [if reused { 7 } else { 0 }]);
                        drop(view);
                        assert_eq!(std::fs::read(&path).unwrap(), before);
                        crate::recover_vhdx(&path).unwrap();
                        crate::recover_vhdx(&path).unwrap();
                        let mut recovered = VhdxWriter::open(&path).unwrap();
                        assert_eq!(recovered.len(), expected);
                        recovered
                            .resize(target, crate::ShrinkPolicy::AllowDataLoss)
                            .unwrap();
                        recovered.read_exact_at(0, &mut out).unwrap();
                        assert_eq!(out, [if reused { 7 } else { 0 }]);
                    }
                }
            }
        }
    }

    #[test]
    fn resize_boundary_payload_prefix_remains_old_capacity_and_regrows_zero() {
        let _process_boundary = crate::test_sync::writer_test();
        for stop in [Stage::CowChunkWritten(65536), Stage::PayloadSynced] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.vhdx");
            let mut writer = VhdxWriter::create(&path, M).unwrap();
            writer.write_all_at(0, &vec![7; M as usize]).unwrap();
            assert!(
                writer
                    .resize_with_hook(512, crate::ShrinkPolicy::AllowDataLoss, |index, stage| {
                        if index == usize::MAX && stage == stop {
                            Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "injected zeroing prefix",
                            ))
                        } else {
                            Ok(())
                        }
                    })
                    .is_err()
            );
            assert!(writer.flush().is_err());
            drop(writer);
            crate::recover_vhdx(&path).unwrap();
            let mut writer = VhdxWriter::open(&path).unwrap();
            assert_eq!(writer.len(), M);
            let mut first = [0];
            writer.read_exact_at(0, &mut first).unwrap();
            assert_eq!(first, [7]);
            writer
                .resize(512, crate::ShrinkPolicy::AllowDataLoss)
                .unwrap();
            writer.resize(M, crate::ShrinkPolicy::Reject).unwrap();
            let mut tail = vec![1; M as usize - 512];
            writer.read_exact_at(512, &mut tail).unwrap();
            assert!(tail.iter().all(|&b| b == 0));
        }
    }

    fn size_sector_replay_cases(oracle: bool) {
        for shrink in [false, true] {
            for reused in [false, true] {
                for stop in [
                    Stage::PayloadSynced,
                    Stage::StagedLogSynced,
                    Stage::ActivatedFirstSynced,
                    Stage::ActivatedSecondSynced,
                    Stage::RedoLogSynced,
                    Stage::BatPublished,
                    Stage::ClearFirstSynced,
                    Stage::ClearSecondSynced,
                ] {
                    if reused && stop == Stage::StagedLogSynced {
                        continue;
                    }
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("disk.vhdx");
                    let raw = dir.path().join("disk.raw");
                    let old = if shrink { 1024 } else { 512 };
                    let target = if shrink { 512 } else { 1024 };
                    let mut writer = VhdxWriter::create(&path, old).unwrap();
                    if reused {
                        writer.write_all_at(0, &[7]).unwrap();
                    }
                    assert!(
                        writer
                            .resize_with_hook(
                                target,
                                crate::ShrinkPolicy::AllowDataLoss,
                                |ordinal, stage| {
                                    if ordinal == 0 && stage == stop {
                                        Err(io::Error::new(
                                            io::ErrorKind::Interrupted,
                                            "injected size-sector cut",
                                        ))
                                    } else {
                                        Ok(())
                                    }
                                }
                            )
                            .is_err()
                    );
                    drop(writer);
                    let before = std::fs::read(&path).unwrap();
                    let view =
                        Vhdx::open_recovered(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
                    let committed = matches!(
                        stop,
                        Stage::RedoLogSynced
                            | Stage::BatPublished
                            | Stage::ClearFirstSynced
                            | Stage::ClearSecondSynced
                    ) || (!reused
                        && matches!(
                            stop,
                            Stage::ActivatedFirstSynced | Stage::ActivatedSecondSynced
                        ));
                    let expected_length = if committed { target } else { old };
                    assert_eq!(view.len(), expected_length);
                    let mut expected = vec![0; expected_length as usize];
                    view.read_exact_at(0, &mut expected).unwrap();
                    assert_eq!(expected[0], if reused { 7 } else { 0 });
                    assert!(expected[1..].iter().all(|&b| b == 0));
                    drop(view);
                    assert_eq!(std::fs::read(&path).unwrap(), before);
                    if oracle {
                        let check = std::process::Command::new("qemu-img")
                            .args(["check", "-r", "all", "-f", "vhdx"])
                            .arg(&path)
                            .output()
                            .unwrap();
                        assert!(
                            check.status.success(),
                            "{}",
                            String::from_utf8_lossy(&check.stderr)
                        );
                        let convert = std::process::Command::new("qemu-img")
                            .args(["convert", "-f", "vhdx", "-O", "raw"])
                            .arg(&path)
                            .arg(&raw)
                            .output()
                            .unwrap();
                        assert!(
                            convert.status.success(),
                            "{}",
                            String::from_utf8_lossy(&convert.stderr)
                        );
                        assert_eq!(
                            std::fs::metadata(&raw).unwrap().len(),
                            expected_length,
                            "QEMU native capacity shrink={shrink} reused={reused} stage={stop:?}"
                        );
                        assert_eq!(std::fs::read(&raw).unwrap(), expected);
                    }
                    crate::recover_vhdx(&path).unwrap();
                    let reopened = VhdxWriter::open(&path).unwrap();
                    assert_eq!(reopened.len(), expected_length);
                }
            }
        }
    }
    #[test]
    fn fresh_reused_native_size_sector_recovery_preserves_exact_capacity() {
        let _process_boundary = crate::test_sync::writer_test();
        size_sector_replay_cases(false);
    }
    #[test]
    #[ignore = "requires independent qemu-img size-sector native recovery oracle"]
    fn qemu_replays_fresh_reused_size_sector_with_exact_capacity_and_payload() {
        let _process_boundary = crate::test_sync::subprocess_test();
        size_sector_replay_cases(true);
    }

    #[test]
    fn resize_cache_work_and_sequence_limits_precede_epoch_mutation() {
        let _process_boundary = crate::test_sync::writer_test();
        for kind in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.vhdx");
            let mut writer = VhdxWriter::create(&path, M).unwrap();
            let before = std::fs::read(&path).unwrap();
            let _exhaust = if kind == 0 {
                Some(
                    writer
                        .budget
                        .cache(
                            writer.budget.limits().cache_bytes - writer.budget.usage().cache_bytes,
                        )
                        .unwrap(),
                )
            } else {
                None
            };
            if kind == 1 {
                writer
                    .budget
                    .work(writer.budget.limits().work_items - writer.budget.usage().work_items)
                    .unwrap();
            }
            if kind == 2 {
                writer.state().unwrap().header[8..16]
                    .copy_from_slice(&(u64::MAX - 6).to_le_bytes());
            }
            assert!(writer.resize(2 * M, crate::ShrinkPolicy::Reject).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn torn_native_size_item_is_restored_from_retained_complete_redo() {
        let _process_boundary = crate::test_sync::writer_test();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.vhdx");
        let mut writer = VhdxWriter::create(&path, 512).unwrap();
        writer.write_all_at(0, &[7]).unwrap();
        let size_offset = writer.size_offset;
        assert!(
            writer
                .resize_with_hook(1024, crate::ShrinkPolicy::Reject, |ordinal, stage| {
                    if ordinal == 0 && stage == Stage::BatPublished {
                        let mut torn = std::fs::OpenOptions::new().write(true).open(&path)?;
                        torn.seek(SeekFrom::Start(size_offset))?;
                        torn.write_all(&[255; 4])?;
                        torn.sync_all()?;
                        Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "torn size-sector publication",
                        ))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        drop(writer);
        let before = std::fs::read(&path).unwrap();
        let recovered = Vhdx::open_recovered(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
        assert_eq!(recovered.len(), 1024);
        let mut out = [0];
        recovered.read_exact_at(0, &mut out).unwrap();
        assert_eq!(out, [7]);
        drop(recovered);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        crate::recover_vhdx(&path).unwrap();
        assert_eq!(VhdxWriter::open(&path).unwrap().len(), 1024);
    }

    #[test]
    fn bitmap_initialization_and_partial_sector_publication_recover_every_stage() {
        let _process_boundary = crate::test_sync::writer_test();
        for scenario in 0..4 {
            for stop in [
                Stage::PayloadSynced,
                Stage::StagedLogSynced,
                Stage::ActivatedFirstSynced,
                Stage::ActivatedSecondSynced,
                Stage::RedoLogSynced,
                Stage::MetadataWritten(0),
                Stage::MetadataWritten(1),
                Stage::BatPublished,
                Stage::ClearFirstSynced,
                Stage::ClearSecondSynced,
            ] {
                if stop == Stage::StagedLogSynced && scenario != 0
                    || stop == Stage::MetadataWritten(1) && scenario == 3
                {
                    continue;
                }
                let dir = tempfile::tempdir().unwrap();
                let parent = dir.path().join("base.vhdx");
                let path = dir.path().join("child.vhdx");
                let base = VhdxWriter::create(&parent, 2 * M).unwrap();
                base.write_all_at(0, &vec![7; 2 * M as usize]).unwrap();
                base.flush().unwrap();
                drop(base);
                let parent_before = std::fs::read(&parent).unwrap();
                let writer = VhdxWriter::create_overlay(&path, &parent, &[]).unwrap();
                if scenario >= 2 {
                    writer.write_all_at(3, &[8]).unwrap();
                }
                let offset = if scenario == 2 {
                    M + 3
                } else if scenario == 3 {
                    1027
                } else {
                    3
                };
                let desired_phase = usize::from(scenario == 1);
                let mut phase = 0;
                let mut state = writer.state().unwrap();
                let error = partial(
                    &writer,
                    &mut state,
                    (offset / M) as usize,
                    offset % M,
                    &[9],
                    |stage| {
                        if phase == desired_phase && stage == stop {
                            return Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "injected bitmap redo cut",
                            ));
                        }
                        if stage == Stage::ClearSecondSynced {
                            phase += 1;
                        }
                        Ok(())
                    },
                )
                .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                drop(state);
                assert!(writer.flush().is_err());
                drop(writer);
                let before = std::fs::read(&path).unwrap();
                let view =
                    Vhdx::open_recovered_chain(&path, std::slice::from_ref(&parent)).unwrap();
                let committed = scenario != 0
                    && matches!(
                        stop,
                        Stage::RedoLogSynced
                            | Stage::MetadataWritten(_)
                            | Stage::BatPublished
                            | Stage::ClearFirstSynced
                            | Stage::ClearSecondSynced
                    );
                let mut bytes = [0; 8];
                view.read_exact_at(offset - 3, &mut bytes).unwrap();
                assert_eq!(
                    bytes[3],
                    if committed { 9 } else { 7 },
                    "scenario={scenario} cut={stop:?}"
                );
                assert!(bytes[..3].iter().chain(bytes[4..].iter()).all(|&b| b == 7));
                drop(view);
                assert_eq!(std::fs::read(&path).unwrap(), before);
                assert!(crate::recover_vhdx_chain(&path, &[]).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), before);
                crate::recover_vhdx_chain(&path, std::slice::from_ref(&parent)).unwrap();
                crate::recover_vhdx_chain(&path, std::slice::from_ref(&parent)).unwrap();
                let writer = VhdxWriter::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
                writer.write_all_at(offset, &[9]).unwrap();
                writer.read_exact_at(offset - 3, &mut bytes).unwrap();
                assert_eq!(bytes[3], 9);
                assert!(bytes[..3].iter().chain(bytes[4..].iter()).all(|&b| b == 7));
                assert_eq!(std::fs::read(&parent).unwrap(), parent_before);
            }
        }
    }

    #[test]
    #[ignore = "requires independent qemu-img multi-descriptor native replay oracle"]
    fn qemu_replays_zero_and_multiple_data_descriptors_in_fresh_reused_epochs() {
        let _process_boundary = crate::test_sync::subprocess_test();
        for reused in [false, true] {
            for stop in [
                Stage::PayloadSynced,
                Stage::StagedLogSynced,
                Stage::ActivatedFirstSynced,
                Stage::ActivatedSecondSynced,
                Stage::RedoLogSynced,
                Stage::MetadataWritten(0),
                Stage::MetadataWritten(1),
                Stage::MetadataWritten(2),
                Stage::BatPublished,
                Stage::ClearFirstSynced,
                Stage::ClearSecondSynced,
            ] {
                if reused && stop == Stage::StagedLogSynced {
                    continue;
                }
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("disk.vhdx");
                let raw = dir.path().join("disk.raw");
                let writer = VhdxWriter::create(&path, M).unwrap();
                if reused {
                    writer.write_all_at(0, &[7]).unwrap();
                }
                let mut state = writer.state().unwrap();
                let zero_offset = state.file.metadata().unwrap().len().div_ceil(M) * M;
                let end = zero_offset + M;
                let bat = bat_update(&writer, &mut state, 1, 2).unwrap();
                let target = writer.size_offset / 4096 * 4096;
                let mut size = Box::new([0; 4096]);
                state.file.seek(SeekFrom::Start(target)).unwrap();
                state.file.read_exact(size.as_mut()).unwrap();
                put64(
                    size.as_mut(),
                    (writer.size_offset - target) as usize,
                    M + 512,
                );
                assert!(
                    commit_partial_updates(
                        &writer,
                        &mut state,
                        vec![
                            Update::Zero {
                                offset: zero_offset,
                                length: M
                            },
                            bat,
                            Update::Data {
                                offset: target,
                                body: size
                            }
                        ],
                        end,
                        |stage| if stage == stop {
                            Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "native multi-descriptor cut",
                            ))
                        } else {
                            Ok(())
                        }
                    )
                    .is_err()
                );
                drop(state);
                drop(writer);
                let view = Vhdx::open_recovered(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
                let mut expected = vec![0; view.len() as usize];
                view.read_exact_at(0, &mut expected).unwrap();
                assert_eq!(expected[0], if reused { 7 } else { 0 });
                let length = view.len();
                drop(view);
                let checked = std::process::Command::new("qemu-img")
                    .args(["check", "-r", "all", "-f", "vhdx"])
                    .arg(&path)
                    .output()
                    .unwrap();
                assert!(
                    checked.status.success(),
                    "{}",
                    String::from_utf8_lossy(&checked.stderr)
                );
                let converted = std::process::Command::new("qemu-img")
                    .args(["convert", "-f", "vhdx", "-O", "raw"])
                    .arg(&path)
                    .arg(&raw)
                    .output()
                    .unwrap();
                assert!(
                    converted.status.success(),
                    "{}",
                    String::from_utf8_lossy(&converted.stderr)
                );
                assert_eq!(std::fs::metadata(&raw).unwrap().len(), length);
                assert_eq!(std::fs::read(&raw).unwrap(), expected);
            }
        }
    }

    #[test]
    fn partial_bitmap_torn_log_and_metadata_retain_complete_old_or_new_redo() {
        let _process_boundary = crate::test_sync::writer_test();
        for tear in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("base.vhdx");
            let path = dir.path().join("child.vhdx");
            let base = VhdxWriter::create(&parent, 2 * M).unwrap();
            base.write_all_at(0, &vec![7; 2 * M as usize]).unwrap();
            base.flush().unwrap();
            drop(base);
            let writer = VhdxWriter::create_overlay(&path, &parent, &[]).unwrap();
            writer.write_all_at(3, &[8]).unwrap();
            let mut state = writer.state().unwrap();
            let log = state.log_epoch.unwrap();
            let bitmap = state.bitmaps[0];
            assert!(
                partial(&writer, &mut state, 1, 3, &[9], |stage| {
                    if (tear == 0 && stage == Stage::RedoLogSynced)
                        || (tear != 0 && stage == Stage::BatPublished)
                    {
                        let mut file = std::fs::OpenOptions::new().write(true).open(&path)?;
                        let position = match tear {
                            0 => log.offset + log.slot + 12284,
                            1 => bitmap,
                            _ => writer.bat_offset + 8,
                        };
                        file.seek(SeekFrom::Start(position))?;
                        file.write_all(if tear == 1 { &[255; 4096] } else { &[255; 4] })?;
                        file.sync_all()?;
                        Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "torn multi-target partial bitmap transaction",
                        ))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
            );
            drop(state);
            drop(writer);
            let original = std::fs::read(&path).unwrap();
            let recovered =
                Vhdx::open_recovered_chain(&path, std::slice::from_ref(&parent)).unwrap();
            let mut bytes = vec![0; 2 * M as usize];
            recovered.read_exact_at(0, &mut bytes).unwrap();
            let mut expected = vec![7; 2 * M as usize];
            expected[3] = 8;
            if tear != 0 {
                expected[M as usize + 3] = 9;
            }
            assert_eq!(bytes, expected);
            drop(recovered);
            assert_eq!(std::fs::read(&path).unwrap(), original);
            crate::recover_vhdx_chain(&path, std::slice::from_ref(&parent)).unwrap();
            let writer = VhdxWriter::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
            writer.read_exact_at(0, &mut bytes).unwrap();
            assert_eq!(bytes, expected);
        }
    }

    fn cut(writer: &VhdxWriter, index: usize, stop: Stage) -> io::Result<()> {
        let mut state = writer.state()?;
        allocate(writer, &mut state, index, 37, &[9], |stage| {
            if stage == stop {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "injected native transaction cut",
                ))
            } else {
                Ok(())
            }
        })
    }
    fn check_and_recover(path: &std::path::Path, index: usize, expected: u8) {
        let recovered = Vhdx::open_recovered(Arc::new(RawDisk::open(path).unwrap())).unwrap();
        let mut out = [0];
        recovered
            .read_exact_at(index as u64 * M + 37, &mut out)
            .unwrap();
        assert_eq!(out, [expected]);
        drop(recovered);
        crate::recover_vhdx(path).unwrap();
        let disk = Vhdx::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
        disk.read_exact_at(index as u64 * M + 37, &mut out).unwrap();
        assert_eq!(out, [expected]);
    }
    #[test]
    fn differencing_cow_native_redo_cuts_preserve_inherited_bytes() {
        let _process_boundary = crate::test_sync::writer_test();
        struct Filled;
        impl ReadAt for Filled {
            fn len(&self) -> u64 {
                2 * M
            }
            fn read_exact_at(&self, at: u64, out: &mut [u8]) -> io::Result<()> {
                crate::check_range(at, out.len() as u64, 2 * M)?;
                out.fill(17);
                Ok(())
            }
        }
        for reused in [false, true] {
            for stop in [
                Stage::CowChunkWritten(65536),
                Stage::PayloadSynced,
                Stage::StagedLogSynced,
                Stage::ActivatedFirstSynced,
                Stage::ActivatedSecondSynced,
                Stage::RedoLogSynced,
                Stage::BatPublished,
                Stage::ClearFirstSynced,
                Stage::ClearSecondSynced,
            ] {
                if reused && stop == Stage::StagedLogSynced {
                    continue;
                }
                let dir = tempfile::tempdir().unwrap();
                let parent = dir.path().join("parent.vhdx");
                let child = dir.path().join("child.vhdx");
                crate::create_vhdx(&parent, &Filled).unwrap();
                let original = std::fs::read(&parent).unwrap();
                let writer = VhdxWriter::create_overlay(&child, &parent, &[]).unwrap();
                if reused {
                    writer.write_all_at(37, &[7]).unwrap();
                }
                let index = usize::from(reused);
                assert_eq!(
                    cut(&writer, index, stop).unwrap_err().kind(),
                    io::ErrorKind::Interrupted
                );
                drop(writer);
                let expected = if matches!(
                    stop,
                    Stage::CowChunkWritten(_) | Stage::PayloadSynced | Stage::StagedLogSynced
                ) || (reused
                    && matches!(
                        stop,
                        Stage::ActivatedFirstSynced | Stage::ActivatedSecondSynced
                    )) {
                    17
                } else {
                    9
                };
                let snapshot = std::fs::read(&child).unwrap();
                let recovered =
                    Vhdx::open_recovered_chain(&child, std::slice::from_ref(&parent)).unwrap();
                let mut out = [0; 3];
                recovered
                    .read_exact_at(index as u64 * M + 36, &mut out)
                    .unwrap();
                assert_eq!(out, [17, expected, 17], "cut {stop:?}");
                drop(recovered);
                assert_eq!(std::fs::read(&child).unwrap(), snapshot);
                assert!(crate::vhdx_recover::recover_vhdx_chain(&child, &[]).is_err());
                assert_eq!(std::fs::read(&child).unwrap(), snapshot);
                crate::vhdx_recover::recover_vhdx_chain(&child, std::slice::from_ref(&parent))
                    .unwrap();
                crate::vhdx_recover::recover_vhdx_chain(&child, std::slice::from_ref(&parent))
                    .unwrap();
                let image = Vhdx::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
                image
                    .read_exact_at(index as u64 * M + 36, &mut out)
                    .unwrap();
                assert_eq!(out, [17, expected, 17]);
                assert_eq!(std::fs::read(&parent).unwrap(), original);
            }
        }
    }
    #[test]
    fn first_and_reused_log_transactions_recover_at_each_persistence_boundary() {
        let _process_boundary = crate::test_sync::writer_test();
        for reused in [false, true] {
            for stop in [
                Stage::PayloadSynced,
                Stage::StagedLogSynced,
                Stage::ActivatedFirstSynced,
                Stage::ActivatedSecondSynced,
                Stage::RedoLogSynced,
                Stage::BatPublished,
                Stage::ClearFirstSynced,
                Stage::ClearSecondSynced,
            ] {
                if reused && stop == Stage::StagedLogSynced {
                    continue;
                }
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("disk");
                let writer = VhdxWriter::create(&path, 2 * M).unwrap();
                if reused {
                    writer.write_all_at(37, &[7]).unwrap();
                }
                let index = usize::from(reused);
                assert_eq!(
                    cut(&writer, index, stop).unwrap_err().kind(),
                    io::ErrorKind::Interrupted
                );
                assert!(writer.write_all_at(0, &[1]).is_err());
                assert!(writer.read_exact_at(0, &mut [0]).is_err());
                assert!(writer.flush().is_err());
                drop(writer);
                let published = if reused {
                    !matches!(
                        stop,
                        Stage::PayloadSynced
                            | Stage::ActivatedFirstSynced
                            | Stage::ActivatedSecondSynced
                    )
                } else {
                    !matches!(stop, Stage::PayloadSynced | Stage::StagedLogSynced)
                };
                check_and_recover(&path, index, if published { 9 } else { 0 });
            }
        }
    }
    #[test]
    fn torn_alternate_log_and_partial_bat_use_retained_complete_redo() {
        let _process_boundary = crate::test_sync::writer_test();
        for torn_log in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk");
            let writer = VhdxWriter::create(&path, 2 * M).unwrap();
            writer.write_all_at(37, &[7]).unwrap();
            let mut state = writer.state().unwrap();
            let log = state.log_epoch.unwrap();
            let error = allocate(&writer, &mut state, 1, 37, &[9], |stage| {
                let stop = if torn_log {
                    Stage::RedoLogSynced
                } else {
                    Stage::BatPublished
                };
                if stage != stop {
                    return Ok(());
                }
                let mut torn = std::fs::OpenOptions::new().write(true).open(&path)?;
                if torn_log {
                    torn.seek(SeekFrom::Start(log.offset + log.slot + 4))?;
                    torn.write_all(&[0; 4])?;
                } else {
                    torn.seek(SeekFrom::Start(writer.bat_offset + 4))?;
                    torn.write_all(&[0xff; 2048])?;
                }
                torn.sync_all()?;
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "injected torn metadata",
                ))
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            drop(state);
            drop(writer);
            check_and_recover(&path, 1, if torn_log { 0 } else { 9 });
            let disk = Vhdx::open(Arc::new(RawDisk::open(path).unwrap())).unwrap();
            let mut out = [0];
            disk.read_exact_at(37, &mut out).unwrap();
            assert_eq!(out, [7]);
        }
    }
    #[test]
    #[ignore = "requires independent qemu-img native transaction recovery oracle"]
    fn qemu_recovers_initial_and_reused_interrupted_transactions() {
        let _process_boundary = crate::test_sync::subprocess_test();
        for reused in [false, true] {
            for stop in [
                Stage::PayloadSynced,
                Stage::ActivatedFirstSynced,
                Stage::RedoLogSynced,
                Stage::BatPublished,
                Stage::ClearFirstSynced,
            ] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("disk.vhdx");
                let raw = dir.path().join("disk.raw");
                let writer = VhdxWriter::create(&path, 2 * M).unwrap();
                if reused {
                    writer.write_all_at(37, &[7]).unwrap();
                }
                let index = usize::from(reused);
                cut(&writer, index, stop).unwrap_err();
                drop(writer);
                let recovered =
                    Vhdx::open_recovered(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
                let mut expected = vec![0; 2 * M as usize];
                recovered.read_exact_at(0, &mut expected).unwrap();
                drop(recovered);
                assert!(
                    std::process::Command::new("qemu-img")
                        .args(["check", "-r", "all", "-f", "vhdx"])
                        .arg(&path)
                        .status()
                        .unwrap()
                        .success()
                );
                assert!(
                    std::process::Command::new("qemu-img")
                        .args(["convert", "-f", "vhdx", "-O", "raw"])
                        .arg(&path)
                        .arg(&raw)
                        .status()
                        .unwrap()
                        .success()
                );
                assert_eq!(
                    std::fs::read(raw).unwrap(),
                    expected,
                    "reused={reused}, stage={stop:?}"
                );
            }
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn zero_bat_discard_recovers_first_and_reused_epochs_without_parent_mutation() {
        let _process_boundary = crate::test_sync::writer_test();
        for overlay in [false, true] {
            for reused in [false, true] {
                for stop in [
                    Stage::PayloadSynced,
                    Stage::StagedLogSynced,
                    Stage::ActivatedFirstSynced,
                    Stage::ActivatedSecondSynced,
                    Stage::RedoLogSynced,
                    Stage::BatPublished,
                    Stage::ClearFirstSynced,
                    Stage::ClearSecondSynced,
                ] {
                    if reused && stop == Stage::StagedLogSynced {
                        continue;
                    }
                    let dir = tempfile::tempdir().unwrap();
                    let parent = dir.path().join("parent.vhdx");
                    let path = dir.path().join("disk.vhdx");
                    let writer = if overlay {
                        let p = VhdxWriter::create(&parent, 2 * M).unwrap();
                        p.write_all_at(0, &vec![17; (2 * M) as usize]).unwrap();
                        p.flush().unwrap();
                        drop(p);
                        VhdxWriter::create_overlay(&path, &parent, &[]).unwrap()
                    } else {
                        let w = VhdxWriter::create(&path, 2 * M).unwrap();
                        w.write_all_at(37, &[7]).unwrap();
                        w.flush().unwrap();
                        drop(w);
                        VhdxWriter::open(&path).unwrap()
                    };
                    let original = if overlay {
                        Some(std::fs::read(&parent).unwrap())
                    } else {
                        None
                    };
                    if reused {
                        writer.write_all_at(M + 37, &[9]).unwrap();
                    }
                    let mut state = writer.state().unwrap();
                    assert_eq!(
                        discard(&writer, &mut state, 0, |stage| {
                            if stage == stop {
                                Err(io::ErrorKind::Interrupted.into())
                            } else {
                                Ok(())
                            }
                        })
                        .unwrap_err()
                        .kind(),
                        io::ErrorKind::Interrupted
                    );
                    drop(state);
                    drop(writer);
                    let expected = if matches!(stop, Stage::PayloadSynced | Stage::StagedLogSynced)
                        || (reused
                            && matches!(
                                stop,
                                Stage::ActivatedFirstSynced | Stage::ActivatedSecondSynced
                            )) {
                        if overlay { 17 } else { 7 }
                    } else {
                        0
                    };
                    let disk = if overlay {
                        Vhdx::open_recovered_chain(&path, std::slice::from_ref(&parent)).unwrap()
                    } else {
                        Vhdx::open_recovered(Arc::new(RawDisk::open(&path).unwrap())).unwrap()
                    };
                    let mut bytes = [0];
                    disk.read_exact_at(37, &mut bytes).unwrap();
                    assert_eq!(
                        bytes,
                        [expected],
                        "overlay {overlay} reused {reused} stop {stop:?}"
                    );
                    drop(disk);
                    if overlay {
                        crate::vhdx_recover::recover_vhdx_chain(
                            &path,
                            std::slice::from_ref(&parent),
                        )
                        .unwrap();
                        assert_eq!(std::fs::read(&parent).unwrap(), original.unwrap());
                    } else {
                        crate::recover_vhdx(&path).unwrap();
                    }
                }
            }
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn complete_partial_bitmap_block_can_be_zero_masked_without_changing_bitmap() {
        let _process_boundary = crate::test_sync::writer_test();
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent.vhdx");
        let path = dir.path().join("child.vhdx");
        let p = VhdxWriter::create(&parent, 2 * M).unwrap();
        p.write_all_at(0, &vec![17; (2 * M) as usize]).unwrap();
        p.flush().unwrap();
        drop(p);
        let original = std::fs::read(&parent).unwrap();
        let w = VhdxWriter::create_overlay(&path, &parent, &[]).unwrap();
        w.write_all_at(37, &[7]).unwrap();
        let mut state = w.state().unwrap();
        let payload = state.map[0];
        let bat = w.bat_offset;
        let bitmap = state.file.metadata().unwrap().len().div_ceil(M) * M;
        state.file.set_len(bitmap + M).unwrap();
        state.file.seek(SeekFrom::Start(bitmap)).unwrap();
        state.file.write_all(&[1]).unwrap();
        state.file.seek(SeekFrom::Start(bat)).unwrap();
        state.file.write_all(&(payload | 7).to_le_bytes()).unwrap();
        state.file.seek(SeekFrom::Start(bat + 4096 * 8)).unwrap();
        state.file.write_all(&(bitmap | 6).to_le_bytes()).unwrap();
        state.file.sync_all().unwrap();
        drop(state);
        drop(w);
        let w = VhdxWriter::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
        w.discard(0, M, crate::DiscardPolicy::RequireDeallocation)
            .unwrap();
        let mut bytes = vec![1; M as usize];
        w.read_exact_at(0, &mut bytes).unwrap();
        assert!(bytes.iter().all(|b| *b == 0));
        w.flush().unwrap();
        drop(w);
        let image = Vhdx::open_chain(&path, std::slice::from_ref(&parent)).unwrap();
        image.read_exact_at(0, &mut bytes).unwrap();
        assert!(bytes.iter().all(|b| *b == 0));
        assert_eq!(std::fs::read(&parent).unwrap(), original);
        let source = RawDisk::open(&path).unwrap();
        let mut first = [0];
        source.read_exact_at(bitmap, &mut first).unwrap();
        assert_eq!(first, [1]);
    }
    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "requires independent qemu-img native discard replay oracle"]
    fn qemu_replays_zero_bat_after_fresh_and_reused_log_interruption() {
        let _process_boundary = crate::test_sync::subprocess_test();
        for reused in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.vhdx");
            let raw = dir.path().join("disk.raw");
            let w = VhdxWriter::create(&path, 2 * M).unwrap();
            w.write_all_at(37, &[9]).unwrap();
            w.flush().unwrap();
            drop(w);
            let w = VhdxWriter::open(&path).unwrap();
            if reused {
                w.write_all_at(M + 37, &[7]).unwrap();
            }
            let stop = if reused {
                Stage::RedoLogSynced
            } else {
                Stage::ActivatedFirstSynced
            };
            let mut state = w.state().unwrap();
            assert_eq!(
                discard(&w, &mut state, 0, |stage| {
                    if stage == stop {
                        Err(io::ErrorKind::Interrupted.into())
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err()
                .kind(),
                io::ErrorKind::Interrupted
            );
            drop(state);
            drop(w);
            assert!(
                std::process::Command::new("qemu-img")
                    .args(["check", "-r", "all", "-f", "vhdx"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(
                std::process::Command::new("qemu-img")
                    .args(["convert", "-f", "vhdx", "-O", "raw"])
                    .arg(&path)
                    .arg(&raw)
                    .status()
                    .unwrap()
                    .success()
            );
            let bytes = std::fs::read(raw).unwrap();
            assert!(bytes[..M as usize].iter().all(|b| *b == 0));
            assert_eq!(bytes[(M + 37) as usize], if reused { 7 } else { 0 });
        }
    }
}
