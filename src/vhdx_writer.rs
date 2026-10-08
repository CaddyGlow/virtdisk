//! Exclusive payload writes with native logged sparse allocation.
#[path = "vhdx_allocate.rs"]
mod allocator;
use crate::{CacheReservation, ReadAt, ReadBudget, Vhdx, check_range};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};
struct FileReader(Mutex<File>, u64);
impl ReadAt for FileReader {
    fn len(&self) -> u64 {
        self.1
    }
    fn read_exact_at(&self, at: u64, out: &mut [u8]) -> io::Result<()> {
        check_range(at, out.len() as u64, self.1)?;
        let mut file = self
            .0
            .lock()
            .map_err(|_| io::Error::other("VHDX source mutex poisoned"))?;
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(out)
    }
}
struct State {
    file: File,
    header: [u8; 4096],
    active: u64,
    epoch: bool,
    failed: bool,
    map: Vec<u64>,
    zero_masks: Vec<bool>,
    states: Vec<u8>,
    bitmaps: Vec<u64>,
    log_epoch: Option<allocator::LogEpoch>,
}
/// An exclusively locked writable clean VHDX with optional authorized parents.
///
/// Supports bounded arbitrary-byte positional payload reads/writes and zeroing.
/// Sparse writes use native BAT redo; authorized child writes use logged sector bitmaps.
/// Native standalone dynamic resize fits the retained BAT arena; run `recover_vhdx` or `recover_vhdx_chain` explicitly
/// before opening an image with a pending log. Each allocating writer epoch
/// appends a fresh 1 MiB log region; old regions await compaction. Header UUID epochs are persisted through both redundant headers
/// before the first payload mutation on each open. A header-update error makes
/// subsequent mutations fail until the image is reopened. Payload writes may
/// partially complete on I/O failure; call [`Self::flush`] for persistence.
/// Cooperative writers are excluded by an OS file lock; external mutation must
/// otherwise be excluded by the caller. Operations are serialized. The writer
/// deliberately does not implement immutable-source `ReadAt`.
pub struct VhdxWriter {
    state: Mutex<State>,
    length: u64,
    block: u64,
    logical_sector: u32,
    physical_sector: u32,
    bat_offset: u64,
    bat_length: u64,
    size_offset: u64,
    dynamic: bool,
    budget: ReadBudget,
    _cache: CacheReservation,
    _zero_cache: CacheReservation,
    base: Option<Vhdx>,
    parent: Option<Arc<Vhdx>>,
    _partial_cache: CacheReservation,
}
impl VhdxWriter {
    pub(crate) fn container_size(&self) -> Option<u64> {
        self.state
            .lock()
            .ok()?
            .file
            .metadata()
            .ok()
            .map(|metadata| metadata.len())
    }

    /// Whether this writer retains an explicitly authorized native parent chain.
    pub fn has_parent(&self) -> bool {
        self.base.is_some()
    }
    /// Canonical direct parent path retained during authorized writable opening.
    pub fn resolved_parent_path(&self) -> Option<std::path::PathBuf> {
        self.base.as_ref().and_then(Vhdx::resolved_parent_path)
    }
    pub(crate) fn native_resize_supported(&self) -> bool {
        cfg!(target_os = "linux")
            && self.dynamic
            && self.base.is_none()
            && self.size_offset % 4096 <= 4088
            && self.bat_length != 0
    }
    pub(crate) fn native_discard_supported(&self) -> bool {
        cfg!(target_os = "linux") && self.dynamic
    }
    pub(crate) fn geometry(&self) -> (u64, u32, u32, u64) {
        (
            self.length,
            self.logical_sector,
            self.physical_sector,
            self.block,
        )
    }
    /// Create a new sparse dynamic image and open it under an exclusive lock.
    /// Capacity must be positive and 512-byte aligned; metadata is bounded to 64 MiB.
    /// Existing paths are never replaced. Creation syncs the image, not its directory.
    pub fn create(path: impl AsRef<Path>, size: u64) -> io::Result<Self> {
        let path = path.as_ref();
        Self::from_locked_file(crate::vhdx_write::create_blank(path, size)?, None)
    }
    /// Open and exclusively lock a regular clean standalone VHDX.
    /// Validation and header selection use the locked file itself, without reopening
    /// its path. Opening alone does not modify any bytes.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        file.try_lock().map_err(io::Error::from)?;
        Self::from_locked_file(file, None)
    }
    pub(crate) fn open_policy(
        path: &Path,
        authorized: Option<&[std::path::PathBuf]>,
        policy: crate::RecoveryPolicy,
    ) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        file.try_lock().map_err(io::Error::from)?;
        let file = crate::vhdx_recover::recover_locked(file, path, authorized, policy)?;
        Self::from_locked_file(file, authorized.map(|paths| (path, paths)))
    }
    /// Open and exclusively lock a child with explicitly authorized parents.
    pub fn open_chain(
        path: impl AsRef<Path>,
        authorized_parent_paths: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        file.try_lock().map_err(io::Error::from)?;
        Self::from_locked_file(file, Some((path, authorized_parent_paths)))
    }
    /// Create a locked native child and authorize its direct parent by argument.
    pub fn create_overlay(
        path: impl AsRef<Path>,
        parent_path: impl AsRef<Path>,
        authorized_parent_paths: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        let path = path.as_ref();
        let parent = std::fs::canonicalize(parent_path)?;
        let file = crate::vhdx_write::create_child(path, &parent, authorized_parent_paths)?;
        let mut approved = authorized_parent_paths.to_vec();
        approved.push(parent);
        Self::from_locked_file(file, Some((path, &approved)))
    }
    fn from_locked_file(
        mut file: File,
        chain: Option<(&Path, &[std::path::PathBuf])>,
    ) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VHDX writer requires a regular file",
            ));
        }
        let source = Arc::new(FileReader(Mutex::new(file.try_clone()?), metadata.len()));
        let image = if let Some((path, authorized)) = chain {
            Vhdx::open_locked_chain(
                source,
                path,
                authorized,
                same_file::Handle::from_file(file.try_clone()?)?,
            )?
        } else {
            Vhdx::open(source)?
        };
        let (_, logical_sector, physical_sector, _) = image.geometry();
        let bat_offset = image.bat_offset();
        let (bat_length, size_offset) = image.resize_geometry();
        let dynamic = !image.leave_blocks_allocated();
        let parent = image.direct_parent();
        let (states, bitmaps) = image.bitmap_state();
        let partial_cache = image
            .budget()
            .unwrap()
            .cache(states.len() as u64 + bitmaps.len() as u64 * 8)?;
        let states = states.to_vec();
        let bitmaps = bitmaps.to_vec();
        let (length, block, map, budget, cache, base) = image.into_writable_parts()?;
        let mut a = [0; 4096];
        let mut b = [0; 4096];
        file.seek(SeekFrom::Start(65536))?;
        file.read_exact(&mut a)?;
        file.seek(SeekFrom::Start(131072))?;
        file.read_exact(&mut b)?;
        let valid = |h: &[u8]| &h[..4] == b"head" && crate::vhdx::checksum(h);
        let seq = |h: &[u8]| u64::from_le_bytes(h[8..16].try_into().unwrap());
        let (header, active) = match (valid(&a), valid(&b)) {
            (true, true) => {
                if seq(&a) > seq(&b) {
                    (a, 65536)
                } else {
                    (b, 131072)
                }
            }
            (true, false) => (a, 65536),
            (false, true) => (b, 131072),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "no valid VHDX writer header",
                ));
            }
        };
        let zero_cache = budget.cache(map.len() as u64)?;
        let zero_masks = vec![false; map.len()];
        Ok(Self {
            state: Mutex::new(State {
                file,
                header,
                active,
                epoch: false,
                failed: false,
                map,
                zero_masks,
                states,
                bitmaps,
                log_epoch: None,
            }),
            length,
            block,
            logical_sector,
            physical_sector,
            bat_offset,
            bat_length,
            size_offset,
            dynamic,
            budget,
            _cache: cache,
            _zero_cache: zero_cache,
            base,
            parent,
            _partial_cache: partial_cache,
        })
    }
    /// Current logical capacity of this writable image.
    pub fn len(&self) -> u64 {
        self.length
    }
    /// Whether the image has zero logical capacity.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
    fn state(&self) -> io::Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("VHDX writer mutex poisoned"))
    }
    fn epoch(state: &mut State) -> io::Result<()> {
        if state.failed {
            return Err(io::Error::other(
                "VHDX header update failed; reopen before further writes",
            ));
        }
        if state.epoch {
            return Ok(());
        }
        let prepare = (|| {
            let sequence = u64::from_le_bytes(state.header[8..16].try_into().unwrap());
            let final_sequence = sequence.checked_add(2).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "VHDX header sequence exhausted")
            })?;
            let mut header = state.header;
            header[16..32].copy_from_slice(&crate::vhdx_write::identity()?);
            header[32..48].copy_from_slice(&crate::vhdx_write::identity()?);
            let inactive = if state.active == 65536 { 131072 } else { 65536 };
            header[8..16].copy_from_slice(&(sequence + 1).to_le_bytes());
            crate::vhdx_write::checksum(&mut header);
            state.file.seek(SeekFrom::Start(inactive))?;
            state.file.write_all(&header)?;
            state.file.sync_all()?;
            header[8..16].copy_from_slice(&final_sequence.to_le_bytes());
            crate::vhdx_write::checksum(&mut header);
            state.file.seek(SeekFrom::Start(state.active))?;
            state.file.write_all(&header)?;
            state.file.sync_all()?;
            state.header = header;
            state.epoch = true;
            Ok(())
        })();
        if prepare.is_err() {
            state.failed = true;
        }
        prepare
    }
    fn requires_copy(&self, index: usize, physical: u64, zero_mask: bool) -> bool {
        !zero_mask
            && self
                .base
                .as_ref()
                .is_some_and(|b| b.requires_copy(index, physical))
    }
    fn write_chunks(&self, state: &mut State, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let mut at = offset;
        let mut remaining = bytes;
        while !remaining.is_empty() {
            self.budget.work(1)?;
            let within = at % self.block;
            let count = (self.block - within).min(remaining.len() as u64) as usize;
            let index = (at / self.block) as usize;
            if state.failed {
                return Err(io::Error::other(
                    "VHDX metadata transaction failed; recover and reopen before writes",
                ));
            }
            if self.parent.is_some() && matches!(state.states[index], 0 | 7) {
                let page_bytes = 32768 * u64::from(self.logical_sector);
                let count = count.min((page_bytes - at % page_bytes) as usize);
                allocator::partial(self, state, index, within, &remaining[..count], |_| Ok(()))?;
                at += count as u64;
                remaining = &remaining[count..];
                continue;
            }
            if self.requires_copy(index, state.map[index], state.zero_masks[index])
                || state.map[index] == 0
            {
                if self.requires_copy(index, state.map[index], state.zero_masks[index])
                    || remaining[..count].iter().any(|&v| v != 0)
                {
                    allocator::allocate(self, state, index, within, &remaining[..count], |_| {
                        Ok(())
                    })?;
                }
            } else {
                Self::epoch(state)?;
                let physical = state.map[index] + within;
                state.file.seek(SeekFrom::Start(physical))?;
                state.file.write_all(&remaining[..count])?;
            }
            at += count as u64;
            remaining = &remaining[count..];
        }
        Ok(())
    }
    /// Write all bytes within capacity, allocating sparse blocks through native redo.
    /// Empty in-range writes do not change the UUID epoch.
    pub fn write_all_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        check_range(offset, bytes.len() as u64, self.length)?;
        if bytes.is_empty() {
            return Ok(());
        }
        let mut state = self.state()?;
        self.write_chunks(&mut state, offset, bytes)
    }
    /// Read exact logical bytes from this writer's current serialized state.
    pub fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        check_range(offset, bytes.len() as u64, self.length)?;
        let mut state = self.state()?;
        if state.failed {
            return Err(io::Error::other(
                "VHDX metadata transaction failed; recover and reopen before reads",
            ));
        }
        self.read_chunks(&mut state, offset, bytes)
    }
    fn read_chunks(&self, state: &mut State, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        let mut at = offset;
        let mut remaining = bytes;
        while !remaining.is_empty() {
            self.budget.work(1)?;
            let index = (at / self.block) as usize;
            let kind = state.states[index];
            let physical = state.map[index];
            let within = at % self.block;
            let count = if kind == 7 {
                (u64::from(self.logical_sector) - at % u64::from(self.logical_sector))
                    .min(remaining.len() as u64)
            } else {
                (self.block - within).min(remaining.len() as u64)
            } as usize;
            let private = if kind == 7 {
                let bit = (at % ((1u64 << 23) * u64::from(self.logical_sector)))
                    / u64::from(self.logical_sector);
                let chunk = (at / ((1u64 << 23) * u64::from(self.logical_sector))) as usize;
                let mut byte = [0];
                self.budget.metadata(1)?;
                state
                    .file
                    .seek(SeekFrom::Start(state.bitmaps[chunk] + bit / 8))?;
                state.file.read_exact(&mut byte)?;
                byte[0] & (1 << (bit % 8)) != 0
            } else {
                kind == 6
            };
            if private {
                state.file.seek(SeekFrom::Start(physical + within))?;
                state.file.read_exact(&mut remaining[..count])?;
            } else if matches!(kind, 0 | 7) && self.parent.is_some() {
                self.parent
                    .as_ref()
                    .unwrap()
                    .read_exact_at(at, &mut remaining[..count])?;
            } else {
                remaining[..count].fill(0);
            }
            at += count as u64;
            remaining = &mut remaining[count..];
        }
        Ok(())
    }
    /// Persistently zero logical bytes on flush, without discarding allocation.
    /// Uses a 64 KiB stack buffer; partial zeroing is possible on I/O failure.
    pub fn write_zeroes(&self, offset: u64, count: u64) -> io::Result<()> {
        check_range(offset, count, self.length)?;
        if count == 0 {
            return Ok(());
        }
        let mut state = self.state()?;
        if state.failed {
            return Err(io::Error::other(
                "VHDX metadata transaction failed; recover and reopen before writes",
            ));
        }
        let zero = [0; 65536];
        let mut at = offset;
        let mut left = count;
        while left != 0 {
            let index = (at / self.block) as usize;
            if state.map[index] == 0 && !(self.parent.is_some() && state.states[index] == 0) {
                self.budget.work(1)?;
                let amount = (self.block - at % self.block).min(left);
                at += amount;
                left -= amount;
                continue;
            }
            let amount = left.min(zero.len() as u64) as usize;
            self.write_chunks(&mut state, at, &zero[..amount])?;
            at += amount as u64;
            left -= amount as u64;
        }
        Ok(())
    }
    /// Discard complete allocation blocks or the final clipped logical block.
    ///
    /// Linux dynamic/differencing native BAT redo changes mappings to ZERO and releases payload
    /// ownership while masking parents. Sector bitmap allocations are retained.
    /// Host holes, physical tail truncation and immediate storage savings are
    /// not promised; a fresh native log may grow the file. Offset must be block
    /// aligned and length block aligned or reach EOF. Unsupported alignment or
    /// platforms require explicit zero fallback. Capacity stays fixed. Multiple
    /// blocks can partially complete on error; recover/reopen after transaction
    /// failure. Partial sector-bitmap mutations are not performed.
    pub fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: crate::DiscardPolicy,
    ) -> io::Result<crate::DiscardResult> {
        check_range(offset, length, self.length)?;
        if length == 0 {
            return Ok(crate::DiscardResult::Zeroed);
        }
        let supported = self.native_discard_supported()
            && offset.is_multiple_of(self.block)
            && (length.is_multiple_of(self.block) || offset + length == self.length);
        if !supported {
            if policy == crate::DiscardPolicy::RequireDeallocation {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native VHDX discard requires Linux and complete allocation units",
                ));
            }
            self.write_zeroes(offset, length)?;
            return Ok(crate::DiscardResult::Zeroed);
        }
        let mut state = self.state()?;
        if state.failed {
            return Err(io::Error::other(
                "VHDX transaction failed; recover and reopen",
            ));
        }
        for index in offset / self.block..(offset + length).div_ceil(self.block) {
            if state.zero_masks[index as usize] {
                continue;
            }
            allocator::discard(self, &mut state, index as usize, |_| Ok(()))?;
        }
        Ok(crate::DiscardResult::Deallocated)
    }

    /// Change standalone dynamic native capacity within the existing BAT arena.
    /// Completed zeroing and BAT updates may remain at the old capacity on failure.
    pub fn resize(&mut self, size: u64, policy: crate::ShrinkPolicy) -> io::Result<()> {
        self.resize_with_hook(size, policy, |_, _| Ok(()))
    }

    fn resize_with_hook(
        &mut self,
        size: u64,
        policy: crate::ShrinkPolicy,
        mut hook: impl FnMut(usize, allocator::Stage) -> io::Result<()>,
    ) -> io::Result<()> {
        if size == self.length {
            return Ok(());
        }
        if !self.native_resize_supported() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VHDX resize requires supported standalone dynamic metadata profile",
            ));
        }
        if size == 0
            || size > 64 * (1u64 << 40)
            || !size.is_multiple_of(u64::from(self.logical_sector))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid VHDX resize capacity",
            ));
        }
        if size < self.length && matches!(policy, crate::ShrinkPolicy::Reject) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VHDX shrink policy rejects reduction",
            ));
        }
        let ratio = (1u64 << 23) * u64::from(self.logical_sector) / self.block;
        let count = size.div_ceil(self.block);
        let entries = count + (count - 1) / ratio;
        if entries * 8 > self.bat_length || self.size_offset % 4096 > 4088 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VHDX resize exceeds BAT arena or metadata-sector profile",
            ));
        }
        let map_cache = self.budget.cache(count * 8)?;
        let zero_cache = self.budget.cache(count)?;
        let partial_cache = self.budget.cache(count + count.div_ceil(ratio) * 8)?;
        let mut state = self.state()?;
        if state.failed {
            return Err(io::Error::other(
                "VHDX transaction failed; recover and reopen",
            ));
        }
        let old_count = state.map.len() as u64;
        let old_entries = old_count + (old_count - 1) / ratio;
        let boundary = size.min(self.length);
        let boundary_bytes = if boundary.is_multiple_of(self.block) {
            0
        } else {
            self.block - boundary % self.block
        };
        let initialization_sectors = if entries > old_entries {
            (entries * 8).div_ceil(4096) - (old_entries * 8) / 4096
        } else {
            0
        };
        let removed = old_count.saturating_sub(count);
        allocator::preflight_metadata_transactions(&state, initialization_sectors + removed + 1)?;
        self.budget
            .work((initialization_sectors + removed + 1) * 20 + boundary_bytes.div_ceil(65536))?;
        if size < self.length && matches!(policy, crate::ShrinkPolicy::RequireZero) {
            let _scan_cache = self.budget.cache(65536)?;
            let mut buffer = [0; 65536];
            let mut cursor = size;
            while cursor < self.length {
                self.budget.work(1)?;
                let index = (cursor / self.block) as usize;
                let within = cursor % self.block;
                let n = (self.length - cursor)
                    .min(self.block - within)
                    .min(buffer.len() as u64) as usize;
                if state.map[index] != 0 {
                    let position = state.map[index] + within;
                    state.file.seek(SeekFrom::Start(position))?;
                    state.file.read_exact(&mut buffer[..n])?;
                    if buffer[..n].iter().any(|&b| b != 0) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "VHDX removed tail contains nonzero data",
                        ));
                    }
                }
                cursor += n as u64;
            }
        }
        let result = (|| {
            Self::epoch(&mut state)?;
            if boundary_bytes != 0 {
                let physical = state.map[(boundary / self.block) as usize];
                if physical != 0 {
                    let _zero_scratch = self.budget.cache(65536)?;
                    let zero = [0; 65536];
                    let mut done = 0;
                    while done < boundary_bytes {
                        let n = (boundary_bytes - done).min(zero.len() as u64) as usize;
                        state
                            .file
                            .seek(SeekFrom::Start(physical + boundary % self.block + done))?;
                        state.file.write_all(&zero[..n])?;
                        done += n as u64;
                        hook(usize::MAX, allocator::Stage::CowChunkWritten(done))?;
                    }
                    state.file.sync_all()?;
                    hook(usize::MAX, allocator::Stage::PayloadSynced)?;
                }
            }
            let mut ordinal = 0;
            if entries > old_entries {
                let start = self.bat_offset + old_entries * 8;
                let end = self.bat_offset + entries * 8;
                let mut sector_offset = start / 4096 * 4096;
                while sector_offset < end {
                    let mut sector = [0; 4096];
                    state.file.seek(SeekFrom::Start(sector_offset))?;
                    state.file.read_exact(&mut sector)?;
                    for position in
                        (start.max(sector_offset)..end.min(sector_offset + 4096)).step_by(8)
                    {
                        let entry = (position - self.bat_offset) / 8;
                        let value = if entry % (ratio + 1) == ratio {
                            0u64
                        } else {
                            2u64
                        };
                        let at = (position - sector_offset) as usize;
                        sector[at..at + 8].copy_from_slice(&value.to_le_bytes());
                    }
                    allocator::metadata_sector(self, &mut state, sector_offset, sector, |stage| {
                        hook(ordinal, stage)
                    })?;
                    ordinal += 1;
                    sector_offset += 4096;
                }
            } else {
                for index in count..old_count {
                    allocator::discard(self, &mut state, index as usize, |stage| {
                        hook(ordinal, stage)
                    })?;
                    ordinal += 1;
                }
            }
            let target = self.size_offset / 4096 * 4096;
            let mut sector = [0; 4096];
            state.file.seek(SeekFrom::Start(target))?;
            state.file.read_exact(&mut sector)?;
            let at = (self.size_offset - target) as usize;
            sector[at..at + 8].copy_from_slice(&size.to_le_bytes());
            allocator::metadata_sector(self, &mut state, target, sector, |stage| {
                hook(ordinal, stage)
            })?;
            Ok(())
        })();
        if let Err(error) = result {
            state.failed = true;
            return Err(error);
        }
        state.map.resize(count as usize, 0);
        state.states.resize(count as usize, 2);
        state.bitmaps.resize(count.div_ceil(ratio) as usize, 0);
        state.zero_masks.resize(count as usize, true);
        drop(state);
        self.length = size;
        self._cache = map_cache;
        self._zero_cache = zero_cache;
        self._partial_cache = partial_cache;
        Ok(())
    }

    /// Persist payloads and file metadata to the backing file.
    pub fn flush(&self) -> io::Result<()> {
        let state = self.state()?;
        if state.failed {
            return Err(io::Error::other(
                "VHDX metadata transaction failed; recover and reopen before flushing",
            ));
        }
        state.file.sync_all()
    }
}
