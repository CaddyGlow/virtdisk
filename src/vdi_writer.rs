use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::{RawWriter, ReadAt, Vdi};

/// Positional writer for standalone VDI 1.1 fixed/dynamic images.
///
/// Opening retains an exclusive operating system lock and validates ownership.
/// Unresolved parents, extra per-block metadata and unsupported profiles are rejected before
/// mutation. Sparse allocation uses a recoverable Linux sidecar transaction. Payload writes may partially complete on failure.
/// A fresh modification UUID is synced before the first payload change after opening
/// or flushing, so old children cannot silently match a modified parent. UUID updates
/// are not atomic on a host crash; callers must recover from failed metadata writes.
/// Native bounded standalone dynamic resize is supported. Native dynamic discard uses the sidecar journal. Pending transactions require
/// reopening this writer for recovery before other access.
/// External programs must respect the advisory lock and keep readers immutable.
pub struct VdiWriter {
    raw: Arc<RawWriter>,
    identity: same_file::Handle,
    parent_paths: Vec<PathBuf>,
    parent: Option<Arc<Vdi>>,
    path: std::path::PathBuf,
    map_offset: u64,
    data_offset: u64,
    size: u64,
    operation: Mutex<State>,
    block: u64,
    dynamic: bool,
}

struct State {
    epoch: bool,
    mappings: Vec<u64>,
    allocated: u32,
    zero_masks: Vec<bool>,
    failed: bool,
}
struct LockedSource {
    raw: Arc<RawWriter>,
    size: u64,
}
impl ReadAt for LockedSource {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        self.raw.read_exact_at(offset, dst)
    }
}

impl VdiWriter {
    pub(crate) fn info_profile(&self) -> (u64, bool) {
        (self.block, self.dynamic)
    }
    /// Create a fully allocated image while retaining its exclusive lock.
    ///
    /// Initial contents read as zero; source capacity is limited to 32 GiB for
    /// bounded ownership validation. Existing paths are never overwritten.
    pub fn create(path: impl AsRef<Path>, size: u64) -> io::Result<Self> {
        Self::create_profile(path, size, false)
    }
    /// Create a sparse standalone dynamic VDI, retaining its exclusive lock.
    /// Journaled allocation currently requires Linux; existing outputs are never replaced.
    pub fn create_sparse(path: impl AsRef<Path>, size: u64) -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI sparse allocation requires Linux",
            ));
        }
        Self::create_profile(path, size, true)
    }
    fn create_profile(path: impl AsRef<Path>, size: u64, sparse: bool) -> io::Result<Self> {
        Self::check_size(size)?;
        let path = path.as_ref();
        use std::{
            fs::OpenOptions,
            io::{Seek, SeekFrom, Write},
        };
        let blocks = size.div_ceil(1048576);
        let data = (512 + blocks * 4).div_ceil(512) * 512;
        let mut header = [0; 512];
        let banner = b"<<< Oracle VM VirtualBox Disk Image >>>\n";
        header[..banner.len()].copy_from_slice(banner);
        for (at, value) in [
            (64, 0xbeda107f),
            (68, 0x10001),
            (72, 400),
            (76, if sparse { 1 } else { 2 }),
            (340, 512),
            (344, data as u32),
            (360, 512),
            (468, 512),
            (376, 1048576),
            (384, blocks as u32),
            (388, if sparse { 0 } else { blocks as u32 }),
        ] {
            header[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        header[368..376].copy_from_slice(&size.to_le_bytes());
        header[392..408].copy_from_slice(&Self::identity()?);
        header[408..424].copy_from_slice(&Self::identity()?);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        file.try_lock().map_err(io::Error::from)?;
        file.set_len(data + if sparse { 0 } else { blocks * 1048576 })?;
        file.seek(SeekFrom::Start(512))?;
        for i in 0..blocks {
            file.write_all(&(if sparse { u32::MAX } else { i as u32 }).to_le_bytes())?;
        }
        file.sync_all()?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.sync_all()?;
        Self::from_raw(
            RawWriter::from_locked_file(file)?,
            path.canonicalize()?,
            &[],
        )
    }

    fn identity() -> io::Result<[u8; 16]> {
        let mut id = [0; 16];
        getrandom::fill(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
        id[7] = (id[7] & 15) | 64;
        id[8] = (id[8] & 63) | 128;
        Ok(id)
    }

    /// Lock, recover and validate an existing standalone private image.
    ///
    /// Unsupported profiles and invalid ownership are rejected before mutation.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_raw(
            RawWriter::open(path.as_ref())?,
            path.as_ref().canonicalize()?,
            &[],
        )
    }

    /// Open a writable child with explicitly ordered direct-parent through base paths.
    /// Parent UUIDs and immutable byte ownership are validated before recovery or mutation.
    pub fn open_chain(path: impl AsRef<Path>, parent_paths: &[PathBuf]) -> io::Result<Self> {
        Self::from_raw(
            RawWriter::open(path.as_ref())?,
            path.as_ref().canonicalize()?,
            parent_paths,
        )
    }
    /// Create a native differencing VDI while retaining its exclusive lock.
    /// Deeper parent paths are explicitly ordered from the selected parent's parent.
    pub fn create_overlay(
        path: impl AsRef<Path>,
        parent_path: impl AsRef<Path>,
        parent_paths: &[PathBuf],
    ) -> io::Result<Self> {
        let parent = Vdi::open_chain(parent_path.as_ref(), parent_paths)?;
        Self::check_size(parent.len())?;
        if parent_paths.len() >= 31 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VDI child exceeds chain depth limit",
            ));
        }
        let file = crate::vdi_write::create_locked_vdi_overlay(
            path.as_ref(),
            parent_path.as_ref(),
            parent_paths,
        )?;
        let mut chain = vec![parent_path.as_ref().canonicalize()?];
        chain.extend_from_slice(parent_paths);
        Self::from_raw(
            RawWriter::from_locked_file(file)?,
            path.as_ref().canonicalize()?,
            &chain,
        )
    }
    /// Whether this writer retains an authorized immutable parent.
    pub fn has_parent(&self) -> bool {
        self.parent.is_some()
    }
    fn check_size(size: u64) -> io::Result<()> {
        if size == 0 || size > 32 * 1024 * 1024 * 1024 || !size.is_multiple_of(512) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI writer capacity must be sector-aligned and at most 32 GiB",
            ));
        }
        Ok(())
    }

    fn from_raw(
        raw: RawWriter,
        path: std::path::PathBuf,
        parent_paths: &[PathBuf],
    ) -> io::Result<Self> {
        let raw = Arc::new(raw);
        let identity = raw.opened_identity()?;
        #[cfg(target_os = "linux")]
        raw.require_single_link_for_journal()?;
        if crate::transaction::pending(&path)? {
            crate::transaction::recover(&path, raw.clone(), &|source| {
                Vdi::open_locked_chain(source, parent_paths, &identity).map(drop)
            })?;
        }
        let source = Arc::new(LockedSource {
            size: raw.len(),
            raw: raw.clone(),
        });
        let disk = Vdi::open_locked_chain(source, parent_paths, &identity)?;
        let size = disk.len();
        Self::check_size(size)?;
        let mut header = [0; 472];
        raw.read_exact_at(0, &mut header)?;
        let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let block = u64::from(word(376));
        let extra = u64::from(word(380));
        let data = u64::from(word(344));
        let count = word(384);
        if extra != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI writer requires blocks without extra metadata",
            ));
        }
        let mut mappings = Vec::with_capacity(count as usize);
        let mut zero_masks = Vec::with_capacity(count as usize);
        for i in 0..count {
            let mut entry = [0; 4];
            raw.read_exact_at(u64::from(word(340)) + u64::from(i) * 4, &mut entry)?;
            let entry = u32::from_le_bytes(entry);
            zero_masks.push(entry == u32::MAX - 1);
            if entry >= u32::MAX - 1 {
                mappings.push(0);
            } else {
                mappings.push(data + u64::from(entry) * block);
            }
        }
        Ok(Self {
            raw,
            identity,
            parent_paths: parent_paths.to_vec(),
            parent: disk.parent_reader(),
            path,
            map_offset: u64::from(word(340)),
            data_offset: data,
            size,
            operation: Mutex::new(State {
                epoch: false,
                mappings,
                allocated: word(388),
                zero_masks,
                failed: false,
            }),
            block,
            dynamic: word(76) != 2,
        })
    }

    /// Fixed virtual capacity in bytes.
    pub fn len(&self) -> u64 {
        self.size
    }
    /// Whether virtual capacity is zero.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    fn operation(&self) -> io::Result<std::sync::MutexGuard<'_, State>> {
        let state = self
            .operation
            .lock()
            .map_err(|_| io::Error::other("VDI writer mutex poisoned"))?;
        if state.failed {
            return Err(io::Error::other(
                "VDI transaction failed; reopen for recovery",
            ));
        }
        Ok(state)
    }

    fn begin_mutation(&self, state: &mut State) -> io::Result<()> {
        if !state.epoch {
            self.raw.write_all_at(408, &Self::identity()?)?;
            self.raw.flush()?;
            state.epoch = true;
        }
        Ok(())
    }

    fn allocate(
        &self,
        index: usize,
        within: u64,
        bytes: &[u8],
        state: &mut State,
        cut: Option<usize>,
    ) -> io::Result<()> {
        use crate::transaction::{self, Patch, Record};
        self.raw.require_single_link_for_journal()?;
        if !self.dynamic || self.block > 1048576 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported VDI allocation block profile",
            ));
        }
        let start = self.data_offset + u64::from(state.allocated) * self.block;
        if self.raw.len() != start {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI allocation requires exact owned physical tail",
            ));
        }
        let source = LockedSource {
            raw: self.raw.clone(),
            size: self.raw.len(),
        };
        let digest = transaction::digest_reader(&source)?;
        let mut payload = vec![0; self.block as usize];
        if !state.zero_masks[index]
            && let Some(parent) = &self.parent
        {
            let offset = index as u64 * self.block;
            let count = (self.size - offset).min(self.block) as usize;
            parent.read_exact_at(offset, &mut payload[..count])?;
        }
        payload[within as usize..within as usize + bytes.len()].copy_from_slice(bytes);
        let mut old_count = vec![0; 4];
        self.raw.read_exact_at(388, &mut old_count)?;
        let map_position = self.map_offset + index as u64 * 4;
        let mut old_map = vec![0; 4];
        self.raw.read_exact_at(map_position, &mut old_map)?;
        let record = Record {
            original_length: self.raw.len(),
            final_length: start + self.block,
            original_digest: digest,
            patches: vec![
                Patch {
                    order: 2,
                    offset: 388,
                    old: old_count,
                    new: (state.allocated + 1).to_le_bytes().to_vec(),
                },
                Patch {
                    order: 1,
                    offset: map_position,
                    old: old_map,
                    new: state.allocated.to_le_bytes().to_vec(),
                },
                Patch {
                    order: 0,
                    offset: start,
                    old: vec![],
                    new: payload,
                },
            ],
        };
        if let Err(error) =
            transaction::commit(&self.path, self.raw.clone(), record, cut, &|source| {
                Vdi::open_locked_chain(source, &self.parent_paths, &self.identity).map(drop)
            })
        {
            state.failed = true;
            return Err(error);
        }
        state.mappings[index] = start;
        state.zero_masks[index] = false;
        state.allocated += 1;
        Ok(())
    }

    /// Write a bounded logical range, allocating sparse blocks through the journal.
    pub fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        crate::check_range(offset, data.len() as u64, self.size)?;
        let mut state = self.operation()?;
        if !data.is_empty() {
            let first = offset / self.block;
            let last = (offset + data.len() as u64 - 1) / self.block;
            if state.mappings[first as usize..=last as usize].contains(&0) {
                self.raw.require_single_link_for_journal()?;
                if self.raw.len() != self.data_offset + u64::from(state.allocated) * self.block {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "VDI allocation requires exact owned physical tail",
                    ));
                }
                if self.block > 1048576 {
                    return Err(io::ErrorKind::Unsupported.into());
                }
            }
            self.begin_mutation(&mut state)?;
        }
        let mut done = 0;
        while done < data.len() {
            let position = offset + done as u64;
            let count =
                (self.block - position % self.block).min((data.len() - done) as u64) as usize;
            let index = (position / self.block) as usize;
            if state.mappings[index] == 0 {
                self.allocate(
                    index,
                    position % self.block,
                    &data[done..done + count],
                    &mut state,
                    None,
                )?;
            }
            self.raw.write_all_at(
                state.mappings[index] + position % self.block,
                &data[done..done + count],
            )?;
            done += count;
        }
        Ok(())
    }

    /// Read a bounded logical range from the currently written image.
    pub fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, dst.len() as u64, self.size)?;
        let state = self.operation()?;
        let mut done = 0;
        while done < dst.len() {
            let position = offset + done as u64;
            let count =
                (self.block - position % self.block).min((dst.len() - done) as u64) as usize;
            let mapping = state.mappings[(position / self.block) as usize];
            if mapping == 0 {
                if let Some(parent) = self
                    .parent
                    .as_ref()
                    .filter(|_| !state.zero_masks[(position / self.block) as usize])
                {
                    parent.read_exact_at(position, &mut dst[done..done + count])?;
                } else {
                    dst[done..done + count].fill(0);
                }
            } else {
                self.raw.read_exact_at(
                    mapping + position % self.block,
                    &mut dst[done..done + count],
                )?;
            }
            done += count;
        }
        Ok(())
    }

    /// Zero a bounded logical range while retaining its allocation.
    pub fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        crate::check_range(offset, length, self.size)?;
        let mut state = self.operation()?;
        if length != 0 {
            let first = (offset / self.block) as usize;
            let last = ((offset + length - 1) / self.block) as usize;
            if self.parent.is_some()
                && (first..=last)
                    .any(|index| state.mappings[index] == 0 && !state.zero_masks[index])
            {
                self.raw.require_single_link_for_journal()?;
                if self.block > 1048576
                    || self.raw.len() != self.data_offset + u64::from(state.allocated) * self.block
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "VDI zero allocation requires supported blocks and exact owned physical tail",
                    ));
                }
            }
            self.begin_mutation(&mut state)?;
        }
        let mut done = 0;
        while done < length {
            let position = offset + done;
            let count = (self.block - position % self.block).min(length - done);
            let index = (position / self.block) as usize;
            if state.mappings[index] == 0 && !state.zero_masks[index] && self.parent.is_some() {
                self.allocate(
                    index,
                    position % self.block,
                    &vec![0; count as usize],
                    &mut state,
                    None,
                )?;
            }
            let mapping = state.mappings[index];
            if mapping != 0 {
                self.raw
                    .write_zeroes(mapping + position % self.block, count)?;
            }
            done += count;
        }
        Ok(())
    }

    /// Discard complete allocation units using recoverable native ZERO mappings.
    /// Dynamic and differencing profiles support blocks up to 1 MiB on Linux.
    /// Interior private allocations are replaced by the last owned allocation,
    /// whose original tail is archived in the journal before truncation. Capacity
    /// stays fixed; inherited bytes are masked and parents remain unchanged.
    /// Offset must be block aligned; the final clipped logical block is allowed.
    /// Each unit is a separate bounded transaction, so errors can leave a completed
    /// prefix. Unsupported profiles require explicit zero fallback, reported Zeroed.
    pub fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: crate::DiscardPolicy,
    ) -> io::Result<crate::DiscardResult> {
        crate::check_range(offset, length, self.size)?;
        if length == 0 {
            return Ok(crate::DiscardResult::Zeroed);
        }
        let mut state = self.operation()?;
        let supported = self.dynamic
            && self.block <= 1048576
            && cfg!(target_os = "linux")
            && offset.is_multiple_of(self.block)
            && (length.is_multiple_of(self.block) || offset + length == self.size);
        let preflight = if supported {
            self.raw.require_single_link_for_journal().and_then(|()| {
                if self.raw.len() != self.data_offset + u64::from(state.allocated) * self.block {
                    Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "VDI discard requires exact owned physical tail",
                    ))
                } else {
                    Ok(())
                }
            })
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI native discard requires whole supported dynamic allocation units",
            ))
        };
        if let Err(error) = preflight {
            if policy == crate::DiscardPolicy::AllowZeroFallback
                && error.kind() == io::ErrorKind::Unsupported
            {
                drop(state);
                self.write_zeroes(offset, length)?;
                return Ok(crate::DiscardResult::Zeroed);
            }
            return Err(error);
        }
        let first = (offset / self.block) as usize;
        let end = (offset + length).div_ceil(self.block) as usize;
        // Validate the complete request before changing the UUID epoch.
        for index in first..end {
            let physical = state.mappings[index];
            if physical != 0
                && (physical < self.data_offset
                    || (physical - self.data_offset) / self.block >= u64::from(state.allocated))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "VDI discard allocation ownership changed",
                ));
            }
        }
        if (first..end).any(|i| !state.zero_masks[i]) {
            self.begin_mutation(&mut state)?;
        }
        for index in first..end {
            self.discard_unit(index, &mut state, None)?;
        }
        Ok(crate::DiscardResult::Deallocated)
    }
    fn discard_unit(&self, index: usize, state: &mut State, cut: Option<usize>) -> io::Result<()> {
        use crate::transaction::{self, Patch, Record};
        if state.zero_masks[index] {
            return Ok(());
        }
        let physical = state.mappings[index];
        let old_length = self.raw.len();
        let source = LockedSource {
            raw: self.raw.clone(),
            size: old_length,
        };
        let mut patches = Vec::new();
        let map_position = self.map_offset + index as u64 * 4;
        let mut old_map = vec![0; 4];
        self.raw.read_exact_at(map_position, &mut old_map)?;
        let mut owner = None;
        let mut final_length = old_length;
        let mut count = state.allocated;
        if physical != 0 {
            let tail = self.data_offset + u64::from(state.allocated - 1) * self.block;
            let mut tail_bytes = vec![0; self.block as usize];
            self.raw.read_exact_at(tail, &mut tail_bytes)?;
            if physical != tail {
                let tail_owner =
                    state
                        .mappings
                        .iter()
                        .position(|&p| p == tail)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "VDI discard tail has no owner",
                            )
                        })?;
                owner = Some(tail_owner);
                let mut old = vec![0; self.block as usize];
                self.raw.read_exact_at(physical, &mut old)?;
                patches.push(Patch {
                    order: 0,
                    offset: physical,
                    old,
                    new: tail_bytes.clone(),
                });
                let position = self.map_offset + tail_owner as u64 * 4;
                let mut old = vec![0; 4];
                self.raw.read_exact_at(position, &mut old)?;
                patches.push(Patch {
                    order: 1,
                    offset: position,
                    old,
                    new: (((physical - self.data_offset) / self.block) as u32)
                        .to_le_bytes()
                        .to_vec(),
                });
            }
            let order = patches.len() as u32;
            patches.push(Patch {
                order,
                offset: map_position,
                old: old_map,
                new: (u32::MAX - 1).to_le_bytes().to_vec(),
            });
            let mut old = vec![0; 4];
            self.raw.read_exact_at(388, &mut old)?;
            count -= 1;
            patches.push(Patch {
                order: order + 1,
                offset: 388,
                old,
                new: count.to_le_bytes().to_vec(),
            });
            final_length = tail;
            patches.push(Patch {
                order: order + 2,
                offset: tail,
                old: tail_bytes,
                new: vec![],
            });
        } else {
            patches.push(Patch {
                order: 0,
                offset: map_position,
                old: old_map,
                new: (u32::MAX - 1).to_le_bytes().to_vec(),
            });
        }
        patches.sort_unstable_by_key(|p| p.offset);
        let record = Record {
            original_length: old_length,
            final_length,
            original_digest: transaction::digest_reader(&source)?,
            patches,
        };
        if let Err(error) =
            transaction::commit(&self.path, self.raw.clone(), record, cut, &|source| {
                Vdi::open_locked_chain(source, &self.parent_paths, &self.identity).map(drop)
            })
        {
            state.failed = true;
            return Err(error);
        }
        if let Some(owner) = owner {
            state.mappings[owner] = physical;
        }
        state.mappings[index] = 0;
        state.zero_masks[index] = true;
        state.allocated = count;
        Ok(())
    }
    /// Change native standalone dynamic capacity. Allocated images must fit their
    /// existing map arena or journaled allocation-unit rotations. Shrink may complete a
    /// deallocated prefix before an I/O failure, with the old capacity retained.
    pub fn resize(&mut self, size: u64, policy: crate::ShrinkPolicy) -> io::Result<()> {
        self.resize_inner(size, policy, None)
    }

    fn resize_inner(
        &mut self,
        size: u64,
        policy: crate::ShrinkPolicy,
        cut: Option<usize>,
    ) -> io::Result<()> {
        self.resize_staged(size, policy, cut, None)
    }

    fn resize_staged(
        &mut self,
        size: u64,
        policy: crate::ShrinkPolicy,
        cut: Option<usize>,
        rotation_cut: Option<(usize, usize)>,
    ) -> io::Result<()> {
        use crate::transaction::{self, Patch, Record};
        if size == self.size {
            return Ok(());
        }
        Self::check_size(size)?;
        let mut state = self.operation()?;
        if !cfg!(target_os = "linux")
            || !self.dynamic
            || self.parent.is_some()
            || self.block > 1 << 20
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI resize requires standalone bounded dynamic Linux profile",
            ));
        }
        if size < self.size && matches!(policy, crate::ShrinkPolicy::Reject) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VDI shrink policy rejects reduction",
            ));
        }
        let count = size.div_ceil(self.block) as usize;
        let map_end = self.map_offset + count as u64 * 4;
        if count as u64 * 4 > 1 << 20 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI resize block map exceeds bounded arena",
            ));
        }
        let mut data_offset = if map_end > self.data_offset {
            map_end.div_ceil(512) * 512
        } else {
            self.data_offset
        };
        let relocating = data_offset != self.data_offset && state.allocated != 0;
        let advance = if relocating {
            (map_end - self.data_offset).div_ceil(self.block) * self.block
        } else {
            data_offset - self.data_offset
        };
        if self.data_offset + advance > u64::from(u32::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VDI relocated data offset exceeds native u32 field",
            ));
        }
        self.raw.require_single_link_for_journal()?;
        if self.raw.len() != self.data_offset + u64::from(state.allocated) * self.block {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "VDI resize requires exact owned container tail",
            ));
        }
        if size < self.size && matches!(policy, crate::ShrinkPolicy::RequireZero) {
            let mut buffer = vec![0; 65536];
            let mut cursor = size;
            while cursor < self.size {
                let index = (cursor / self.block) as usize;
                let within = cursor % self.block;
                let n = (self.size - cursor)
                    .min(self.block - within)
                    .min(buffer.len() as u64) as usize;
                if state.mappings[index] != 0 {
                    self.raw
                        .read_exact_at(state.mappings[index] + within, &mut buffer[..n])?;
                    if buffer[..n].iter().any(|&b| b != 0) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "VDI removed tail contains nonzero data",
                        ));
                    }
                }
                cursor += n as u64;
            }
        }
        if let Err(error) = self.begin_mutation(&mut state) {
            state.failed = true;
            return Err(error);
        }
        // Any failure after a completed prefix requires reopening because cached
        // geometry is published only after the final capacity transaction.
        state.failed = true;
        if relocating {
            let boundary = self.size % self.block;
            if boundary != 0 {
                let physical = state.mappings[(self.size / self.block) as usize];
                if physical != 0 {
                    let position = physical + boundary;
                    let mut old = vec![0; (self.block - boundary) as usize];
                    self.raw.read_exact_at(position, &mut old)?;
                    let new = vec![0; old.len()];
                    self.resize_transaction(
                        &mut state,
                        vec![Patch {
                            order: 0,
                            offset: position,
                            old,
                            new,
                        }],
                        self.raw.len(),
                        rotation_cut.and_then(|(stage, cut)| (stage == usize::MAX).then_some(cut)),
                    )?;
                }
            }
            data_offset = self.data_offset;
            let steps = (map_end - data_offset).div_ceil(self.block) as usize;
            for step in 0..steps {
                let injection =
                    rotation_cut.and_then(|(stage, cut)| (stage == step).then_some(cut));
                data_offset = self.rotate_arena_unit(data_offset, &mut state, injection)?;
            }
        }
        if count < state.mappings.len() {
            for index in count..state.mappings.len() {
                self.discard_unit(index, &mut state, None)?;
            }
        }
        let original_length = self.raw.len();
        let final_length =
            original_length.max(data_offset + u64::from(state.allocated) * self.block);
        let mut patches = Vec::new();
        let mut add = |offset: u64, new: Vec<u8>| -> io::Result<()> {
            let n = original_length.saturating_sub(offset).min(new.len() as u64) as usize;
            let mut old = vec![0; n];
            if n != 0 {
                self.raw.read_exact_at(offset, &mut old)?;
            }
            patches.push(Patch {
                order: patches.len() as u32,
                offset,
                old,
                new,
            });
            Ok(())
        };
        add(368, size.to_le_bytes().to_vec())?;
        add(384, (count as u32).to_le_bytes().to_vec())?;
        if data_offset != self.data_offset && !relocating {
            add(344, (data_offset as u32).to_le_bytes().to_vec())?;
        }
        if count > state.mappings.len() {
            let start = self.map_offset + state.mappings.len() as u64 * 4;
            let end = if final_length > original_length {
                data_offset
            } else {
                map_end
            };
            let mut bytes = vec![0; (end - start) as usize];
            bytes[..(map_end - start) as usize].fill(255);
            add(start, bytes)?;
        }
        let boundary = size.min(self.size);
        if !relocating && !boundary.is_multiple_of(self.block) {
            let physical = state.mappings[(boundary / self.block) as usize];
            if physical != 0 {
                add(
                    physical + boundary % self.block,
                    vec![0; (self.block - boundary % self.block) as usize],
                )?;
            }
        }
        patches.sort_unstable_by_key(|p| p.offset);
        let source = LockedSource {
            raw: self.raw.clone(),
            size: original_length,
        };
        let record = Record {
            original_length,
            final_length,
            original_digest: transaction::digest_reader(&source)?,
            patches,
        };
        if let Err(error) =
            transaction::commit(&self.path, self.raw.clone(), record, cut, &|source| {
                Vdi::open_locked_chain(source, &self.parent_paths, &self.identity).map(drop)
            })
        {
            state.failed = true;
            return Err(error);
        }
        state.failed = false;
        state.mappings.resize(count, 0);
        state.zero_masks.resize(count, false);
        drop(state);
        self.size = size;
        self.data_offset = data_offset;
        Ok(())
    }
    fn resize_transaction(
        &self,
        state: &mut State,
        mut patches: Vec<crate::transaction::Patch>,
        final_length: u64,
        cut: Option<usize>,
    ) -> io::Result<()> {
        use crate::transaction::{self, Record};
        patches.sort_unstable_by_key(|p| p.offset);
        let original_length = self.raw.len();
        let source = LockedSource {
            raw: self.raw.clone(),
            size: original_length,
        };
        let record = Record {
            original_length,
            final_length,
            original_digest: transaction::digest_reader(&source)?,
            patches,
        };
        if let Err(error) =
            transaction::commit(&self.path, self.raw.clone(), record, cut, &|source| {
                Vdi::open_locked_chain(source, &self.parent_paths, &self.identity).map(drop)
            })
        {
            state.failed = true;
            return Err(error);
        }
        Ok(())
    }

    fn rotate_arena_unit(
        &self,
        data_offset: u64,
        state: &mut State,
        cut: Option<usize>,
    ) -> io::Result<u64> {
        use crate::transaction::Patch;
        let original_length = self.raw.len();
        let mut payload = vec![0; self.block as usize];
        self.raw.read_exact_at(data_offset, &mut payload)?;
        let mut old_map = vec![0; state.mappings.len() * 4];
        self.raw.read_exact_at(self.map_offset, &mut old_map)?;
        let mut new_map = old_map.clone();
        for entry in new_map.as_chunks_mut::<4>().0 {
            let index = u32::from_le_bytes(*entry);
            if index < u32::MAX - 1 {
                let rotated = if index == 0 {
                    state.allocated - 1
                } else {
                    index - 1
                };
                entry.copy_from_slice(&rotated.to_le_bytes());
            }
        }
        let new_offset = data_offset + self.block;
        let patches = vec![
            Patch {
                order: 0,
                offset: original_length,
                old: vec![],
                new: payload,
            },
            Patch {
                order: 1,
                offset: self.map_offset,
                old: old_map,
                new: new_map,
            },
            Patch {
                order: 2,
                offset: 344,
                old: (data_offset as u32).to_le_bytes().to_vec(),
                new: (new_offset as u32).to_le_bytes().to_vec(),
            },
        ];
        self.resize_transaction(state, patches, original_length + self.block, cut)?;
        for physical in &mut state.mappings {
            if *physical == data_offset {
                *physical = original_length;
            }
        }
        Ok(new_offset)
    }

    /// Durably flush completed payload writes through the host filesystem.
    pub fn flush(&self) -> io::Result<()> {
        let mut state = self.operation()?;
        self.raw.flush()?;
        state.epoch = false;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod recovery_tests {
    use super::*;

    #[test]
    fn hidden_suffix_zeroing_before_rotation_recovers_every_cut() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in 0..=6 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.vdi");
            let mut writer = VdiWriter::create_sparse(&path, 512).unwrap();
            writer.write_all_at(0, &[7]).unwrap();
            writer.flush().unwrap();
            writer
                .raw
                .write_all_at(writer.data_offset + 512, &vec![9; (1 << 20) - 512])
                .unwrap();
            assert!(
                writer
                    .resize_staged(
                        129 << 20,
                        crate::ShrinkPolicy::Reject,
                        None,
                        Some((usize::MAX, cut))
                    )
                    .is_err()
            );
            drop(writer);
            let mut recovered = VdiWriter::open(&path).unwrap();
            assert_eq!(recovered.len(), 512);
            recovered
                .resize(129 << 20, crate::ShrinkPolicy::Reject)
                .unwrap();
            let mut bytes = vec![1; (1 << 20) - 512];
            recovered.read_exact_at(512, &mut bytes).unwrap();
            assert!(bytes.iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn multi_stage_rotations_keep_old_capacity_recoverable_at_every_cut() {
        let _process_boundary = crate::test_sync::writer_test();
        for stage in [0, 15, 30] {
            for cut in 0..=8 {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("disk.vdi");
                drop(VdiWriter::create_sparse(&path, 512).unwrap());
                let mut header = std::fs::read(&path).unwrap();
                header[376..380].copy_from_slice(&512u32.to_le_bytes());
                std::fs::write(&path, header).unwrap();
                let mut writer = VdiWriter::open(&path).unwrap();
                writer.write_all_at(0, &[7]).unwrap();
                writer.flush().unwrap();
                assert!(
                    writer
                        .resize_staged(
                            2 << 20,
                            crate::ShrinkPolicy::Reject,
                            None,
                            Some((stage, cut))
                        )
                        .is_err()
                );
                assert!(writer.flush().is_err());
                drop(writer);
                let mut recovered = VdiWriter::open(&path).unwrap();
                assert_eq!(recovered.len(), 512);
                let mut out = [0];
                recovered.read_exact_at(0, &mut out).unwrap();
                assert_eq!(out, [7]);
                recovered
                    .resize(2 << 20, crate::ShrinkPolicy::Reject)
                    .unwrap();
                recovered.read_exact_at(0, &mut out).unwrap();
                assert_eq!(out, [7]);
                recovered.write_all_at((2 << 20) - 512, &[8]).unwrap();
                recovered.flush().unwrap();
                drop(recovered);
                let recovered = VdiWriter::open(&path).unwrap();
                recovered.read_exact_at((2 << 20) - 512, &mut out).unwrap();
                assert_eq!(out, [8]);
            }
        }
    }

    #[test]
    fn allocated_arena_relocation_recovers_every_overlapping_patch_cut() {
        let _process_boundary = crate::test_sync::writer_test();
        for target in [129 << 20, 32u64 << 30] {
            for cut in 0..=8 {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("disk.vdi");
                let mut writer = VdiWriter::create_sparse(&path, 512).unwrap();
                writer.write_all_at(0, &[7]).unwrap();
                writer.flush().unwrap();
                writer
                    .raw
                    .write_all_at(writer.data_offset + 512, &vec![9; (1 << 20) - 512])
                    .unwrap();
                assert!(
                    writer
                        .resize_inner(target, crate::ShrinkPolicy::Reject, Some(cut))
                        .is_err()
                );
                assert!(writer.flush().is_err());
                drop(writer);
                let recovered = VdiWriter::open(&path).unwrap();
                assert_eq!(recovered.len(), target);
                let mut first = [0];
                recovered.read_exact_at(0, &mut first).unwrap();
                assert_eq!(first, [7]);
                let mut tail = vec![1; (1 << 20) - 512];
                recovered.read_exact_at(512, &mut tail).unwrap();
                assert!(tail.iter().all(|&b| b == 0));
                recovered.write_all_at(target - 512, &[8]).unwrap();
                recovered.flush().unwrap();
                drop(recovered);
                let recovered = VdiWriter::open(&path).unwrap();
                recovered.read_exact_at(target - 512, &mut first).unwrap();
                assert_eq!(first, [8]);
            }
        }
    }

    #[test]
    fn native_resize_final_transaction_recovers_every_cut() {
        let _process_boundary = crate::test_sync::writer_test();
        for mode in 0..3 {
            for cut in 0..=if mode == 2 { 9 } else { 8 } {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("disk.vdi");
                let mut writer = VdiWriter::create_sparse(&path, 1 << 20).unwrap();
                if mode != 2 {
                    writer.write_all_at(0, &[7]).unwrap();
                }
                writer.flush().unwrap();
                let target = match mode {
                    0 => 512,
                    1 => 3 << 20,
                    _ => 129 << 20,
                };
                assert!(
                    writer
                        .resize_inner(target, crate::ShrinkPolicy::AllowDataLoss, Some(cut))
                        .is_err()
                );
                assert!(writer.flush().is_err());
                drop(writer);
                let recovered = VdiWriter::open(&path).unwrap();
                assert_eq!(recovered.len(), target);
                let mut out = [0];
                recovered.read_exact_at(0, &mut out).unwrap();
                assert_eq!(out, [if mode == 2 { 0 } else { 7 }]);
                recovered.flush().unwrap();
                drop(recovered);
                assert_eq!(VdiWriter::open(&path).unwrap().len(), target);
            }
        }
    }

    #[test]
    fn discard_swap_and_archived_tail_recover_every_cut_with_authorized_overlay() {
        let _process_boundary = crate::test_sync::writer_test();
        const M: u64 = 1 << 20;
        for overlay in [false, true] {
            for cut in 0..=10 {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("disk.vdi");
                let parent = dir.path().join("parent.vdi");
                let writer = if overlay {
                    let base = VdiWriter::create(&parent, 3 * M).unwrap();
                    base.write_all_at(0, &vec![7; 3 * M as usize]).unwrap();
                    base.flush().unwrap();
                    drop(base);
                    VdiWriter::create_overlay(&path, &parent, &[]).unwrap()
                } else {
                    VdiWriter::create_sparse(&path, 3 * M).unwrap()
                };
                writer.write_all_at(0, &vec![3; M as usize]).unwrap();
                if !overlay {
                    writer.write_all_at(M, &vec![4; M as usize]).unwrap();
                }
                writer.write_all_at(2 * M, &vec![8; M as usize]).unwrap();
                writer.flush().unwrap();
                let original_parent = if overlay {
                    std::fs::read(&parent).unwrap()
                } else {
                    vec![]
                };
                let before = std::fs::metadata(&path).unwrap().len();
                {
                    let mut state = writer.operation().unwrap();
                    writer.begin_mutation(&mut state).unwrap();
                    assert!(writer.discard_unit(0, &mut state, Some(cut)).is_err());
                }
                assert!(writer.flush().is_err());
                assert!(writer.read_exact_at(0, &mut [0]).is_err());
                drop(writer);
                if overlay {
                    let interrupted = std::fs::read(&path).unwrap();
                    assert!(VdiWriter::open(&path).is_err());
                    assert_eq!(std::fs::read(&path).unwrap(), interrupted);
                }
                let open = || {
                    if overlay {
                        VdiWriter::open_chain(&path, std::slice::from_ref(&parent))
                    } else {
                        VdiWriter::open(&path)
                    }
                };
                for _ in 0..2 {
                    let recovered = open().unwrap();
                    let mut out = [9; 4];
                    recovered.read_exact_at(0, &mut out).unwrap();
                    assert_eq!(out, [0; 4]);
                    recovered.read_exact_at(M, &mut out).unwrap();
                    assert_eq!(out, [if overlay { 7 } else { 4 }; 4]);
                    recovered.read_exact_at(2 * M, &mut out).unwrap();
                    assert_eq!(out, [8; 4]);
                    assert_eq!(std::fs::metadata(&path).unwrap().len(), before - M);
                }
                if overlay {
                    assert_eq!(std::fs::read(&parent).unwrap(), original_parent);
                }
            }
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn each_transaction_cut_recovers_idempotently_and_pending_reader_fails_closed() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in 0..=8 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.vdi");
            let writer = VdiWriter::create_sparse(&path, 2 * 1048576).unwrap();
            {
                let mut state = writer.operation().unwrap();
                writer.begin_mutation(&mut state).unwrap();
                assert!(
                    writer
                        .allocate(1, 17, &[8; 19], &mut state, Some(cut))
                        .is_err()
                );
            }
            if cut == 0 {
                writer.raw.write_all_at(388, &[1]).unwrap();
                writer
                    .raw
                    .write_all_at(writer.map_offset + 4, &[0; 2])
                    .unwrap();
            }
            assert!(writer.read_exact_at(0, &mut [0; 1]).is_err());
            assert!(Vdi::open(Arc::new(crate::RawDisk::open(&path).unwrap())).is_err());
            drop(writer);
            let writer = VdiWriter::open(&path).unwrap();
            let mut actual = [1; 64];
            writer.read_exact_at(1048576, &mut actual).unwrap();
            assert_eq!(&actual[..17], &[0; 17]);
            assert_eq!(&actual[17..36], &[8; 19]);
            assert_eq!(&actual[36..], &[0; 28]);
            drop(writer);
            let writer = VdiWriter::open(&path).unwrap();
            writer.read_exact_at(1048576, &mut actual).unwrap();
            assert_eq!(&actual[17..36], &[8; 19]);
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn backed_transaction_cuts_require_parent_authorization_before_replay() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in 0..=8 {
            let directory = tempfile::tempdir().unwrap();
            let base = directory.path().join("base.vdi");
            let child = directory.path().join("child.vdi");
            let parent = VdiWriter::create(&base, 2 * 1048576).unwrap();
            parent.write_all_at(0, &vec![5; 2 * 1048576]).unwrap();
            parent.flush().unwrap();
            drop(parent);
            let original_parent = std::fs::read(&base).unwrap();
            let writer = VdiWriter::create_overlay(&child, &base, &[]).unwrap();
            {
                let mut state = writer.operation().unwrap();
                writer.begin_mutation(&mut state).unwrap();
                assert!(
                    writer
                        .allocate(1, 17, &[8; 19], &mut state, Some(cut))
                        .is_err()
                );
            }
            assert!(writer.read_exact_at(0, &mut [0; 1]).is_err());
            drop(writer);
            let interrupted = std::fs::read(&child).unwrap();
            assert!(VdiWriter::open(&child).is_err());
            assert_eq!(std::fs::read(&child).unwrap(), interrupted);
            assert!(crate::transaction::pending(&child).unwrap());
            let writer = VdiWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
            let mut actual = [0; 64];
            writer.read_exact_at(1048576, &mut actual).unwrap();
            assert_eq!(&actual[..17], &[5; 17]);
            assert_eq!(&actual[17..36], &[8; 19]);
            assert_eq!(&actual[36..], &[5; 28]);
            drop(writer);
            assert!(!crate::transaction::pending(&child).unwrap());
            let writer = VdiWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
            writer.read_exact_at(1048576, &mut actual).unwrap();
            assert_eq!(&actual[17..36], &[8; 19]);
            assert_eq!(std::fs::read(&base).unwrap(), original_parent);
        }
    }
}
