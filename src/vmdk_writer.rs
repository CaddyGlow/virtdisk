#[path = "vmdk_flat.rs"]
mod flat;
#[cfg(target_os = "linux")]
#[path = "vmdk_sparse_set.rs"]
mod sparse_set;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::{RawWriter, ReadAt, Vmdk};

/// Positional writer for hosted sparse VMDK v1 and authorized monolithicFlat descriptors.
///
/// Opening retains an exclusive operating system lock and validates ownership.
/// The hosted profile requires 64 KiB private grains, existing 512-entry grain tables and an embedded
/// descriptor with a CID. Sparse allocation uses a recoverable Linux
/// sidecar with payload, redundant and primary mapping updates in that order.
/// A new CID is synced before the first modification after opening or flushing.
/// Payload overwrites remain non-atomic and may partially complete on failure.
/// Parented children require `open_chain`; compressed and dirty native profiles fail.
/// Native standalone capacity changes use bounded staged metadata transactions.
/// Native snapshots and monolithic missing-table creation during ordinary
/// writes are not implemented. Authorized split profiles support bounded table
/// creation through their complete-set coordinator. Failed transactions
/// require reopening for recovery. External programs must respect the advisory lock,
/// sidecar exclusion and immutable-reader contract.
pub struct VmdkWriter {
    raw: Arc<RawWriter>,
    _identity: same_file::Handle,
    path: std::path::PathBuf,
    primary: Vec<u64>,
    redundant: Vec<Option<u64>>,
    cid_offset: u64,
    cid_width: usize,
    size: u64,
    flat: Option<flat::Flat>,
    #[cfg(target_os = "linux")]
    sparse: Option<Box<sparse_set::Sparse>>,
    parent: Option<Arc<Vmdk>>,
    operation: Mutex<State>,
}

struct State {
    mappings: Vec<u64>,
    zero_mask: Vec<bool>,
    epoch: bool,
    failed: bool,
}
struct LockedSource {
    raw: Arc<RawWriter>,
    size: u64,
}

fn resize_descriptor(bytes: &[u8], sectors: u64) -> io::Result<Vec<u8>> {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    let mut text = std::str::from_utf8(&bytes[..end])
        .map_err(|_| io::ErrorKind::InvalidData)?
        .to_owned();
    let mut cursor = 0;
    let mut range = None;
    for line in text.split_inclusive('\n') {
        let mut tokens = line.split_whitespace();
        if tokens.next() == Some("RW") {
            if range.is_some() {
                return Err(io::ErrorKind::Unsupported.into());
            }
            let count = tokens.next().ok_or(io::ErrorKind::InvalidData)?;
            if tokens.next() != Some("SPARSE") {
                return Err(io::ErrorKind::Unsupported.into());
            }
            let start = line.find("RW").unwrap() + 2;
            let start = start + line[start..].len() - line[start..].trim_start().len();
            range = Some(cursor + start..cursor + start + count.len());
        }
        cursor += line.len();
    }
    text.replace_range(
        range.ok_or(io::ErrorKind::Unsupported)?,
        &sectors.to_string(),
    );
    if text.len() > bytes.len() {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut result = vec![0; bytes.len()];
    result[..text.len()].copy_from_slice(text.as_bytes());
    Ok(result)
}

fn descriptor_cid_offset(bytes: &[u8], offset: u64) -> io::Result<u64> {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| io::ErrorKind::InvalidData)?;
    let mut cursor = 0;
    for line in text.split_inclusive('\n') {
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "CID"
        {
            return Ok(offset
                + (cursor + line.find('=').unwrap() + 1 + value.len() - value.trim_start().len())
                    as u64);
        }
        cursor += line.len();
    }
    Err(io::ErrorKind::InvalidData.into())
}
impl ReadAt for LockedSource {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        self.raw.read_exact_at(offset, dst)
    }
}

impl VmdkWriter {
    pub(crate) fn split_sparse_profile(path: &Path) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            sparse_set::matches_profile(path)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            Ok(false)
        }
    }
    pub(crate) fn container_sizes(&self) -> (u64, Option<u64>) {
        let primary = self.raw.len();
        #[cfg(target_os = "linux")]
        if let Some(sparse) = &self.sparse {
            return (primary, sparse.container_set_size());
        }
        let aggregate = self.flat.as_ref().map_or(Some(primary), |flat| {
            primary.checked_add(flat.container_extents_size()?)
        });
        (primary, aggregate)
    }

    pub(crate) fn native_discard_supported(&self) -> bool {
        cfg!(target_os = "linux")
            && !self.is_descriptor()
            && self.raw.len() <= 33 * 1024 * 1024 * 1024
    }

    /// Discard full 64 KiB grains, including a clipped final logical grain.
    ///
    /// Linux hosted sparse standalone and authorized overlays publish native
    /// ZERO mappings with journaled primary/redundant updates. Parent content
    /// remains masked across reads, reopening and later COW writes. Active
    /// payload ownership is released; physical bytes and file length may remain.
    /// Each grain is a separate transaction, so errors can leave a completed
    /// prefix. Partial ranges and descriptor profiles require explicit fallback.
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
        let supported = self.native_discard_supported()
            && offset.is_multiple_of(65536)
            && (length.is_multiple_of(65536) || offset + length == self.size);
        if !supported {
            if policy == crate::DiscardPolicy::RequireDeallocation {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native VMDK discard requires Linux hosted sparse complete grains",
                ));
            }
            self.write_zeroes(offset, length)?;
            return Ok(crate::DiscardResult::Zeroed);
        }
        self.raw.require_single_link_for_journal()?;
        let mut state = self.operation()?;
        for index in offset / 65536..(offset + length).div_ceil(65536) {
            if state.zero_mask[index as usize] && state.mappings[index as usize] == 0 {
                continue;
            }
            self.begin_mutation(&mut state)?;
            self.discard_grain(index as usize, &mut state, None)?;
        }
        Ok(crate::DiscardResult::Deallocated)
    }

    fn discard_grain(&self, index: usize, state: &mut State, cut: Option<usize>) -> io::Result<()> {
        let mut flags = [0; 4];
        self.raw.read_exact_at(8, &mut flags)?;
        let flags = u32::from_le_bytes(flags);
        let mut patches = Vec::new();
        if flags & 4 == 0 {
            patches.push(self.resize_patch(8, (flags | 4).to_le_bytes().to_vec(), 0)?);
        }
        if let Some(offset) = self.redundant[index] {
            patches.push(self.resize_patch(
                offset,
                1u32.to_le_bytes().to_vec(),
                patches.len() as u32,
            )?);
        }
        patches.push(self.resize_patch(
            self.primary[index],
            1u32.to_le_bytes().to_vec(),
            patches.len() as u32,
        )?);
        let mut transaction_index = 0;
        self.resize_commit(
            patches,
            self.raw.len(),
            state,
            &mut transaction_index,
            cut.map(|stage| (0, stage)),
        )?;
        state.mappings[index] = 0;
        state.zero_mask[index] = true;
        Ok(())
    }
    pub(crate) fn native_resize_supported(&self) -> bool {
        cfg!(target_os = "linux")
            && !self.is_descriptor()
            && self.parent.is_none()
            && self.raw.len().is_multiple_of(65536)
    }
    /// Resize a standalone hosted sparse image with explicit shrink policy.
    ///
    /// Linux journal recovery, existing 64 KiB grains and 512-entry tables are
    /// required. Growth prepares fresh table coverage when necessary. Shrink
    /// zeroes the removed logical suffix and releases its mappings, but does
    /// not reclaim host storage or resize guest filesystems. Completed staging
    /// transactions can remain at the old capacity after a later failure;
    /// failed publication requires reopening for recovery.
    pub fn resize(&mut self, new_size: u64, policy: crate::ShrinkPolicy) -> io::Result<()> {
        self.resize_inner(new_size, policy, None)
    }

    fn resize_inner(
        &mut self,
        new_size: u64,
        policy: crate::ShrinkPolicy,
        cut: Option<(usize, usize)>,
    ) -> io::Result<()> {
        Self::check_size(new_size)?;
        if !cfg!(target_os = "linux") || self.is_descriptor() || self.parent.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native VMDK resize requires standalone Linux hosted sparse profile",
            ));
        }
        if new_size == self.size {
            drop(self.operation()?);
            return Ok(());
        }
        if new_size < self.size {
            if policy == crate::ShrinkPolicy::Reject {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "shrink requires an explicit tail-removal policy",
                ));
            }
            if policy == crate::ShrinkPolicy::RequireZero {
                let mut buffer = vec![0; 65536];
                let mut offset = new_size;
                while offset < self.size {
                    let count = (self.size - offset).min(65536) as usize;
                    self.read_exact_at(offset, &mut buffer[..count])?;
                    if buffer[..count].iter().any(|byte| *byte != 0) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "removed VMDK tail contains nonzero content",
                        ));
                    }
                    offset += count as u64;
                }
            }
        }
        let mut header = vec![0; 512];
        self.raw.read_exact_at(0, &mut header)?;
        let value = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
        let descriptor_offset = value(28) * 512;
        let descriptor_length = value(36) * 512;
        let old_overhead = value(64) * 512;
        let old_gd = value(56) * 512;
        let old_rgd = value(48) * 512;
        let count = new_size.div_ceil(65536) as usize;
        let tables = (count as u64).div_ceil(512);
        let old_tables = self.size.div_ceil(65536).div_ceil(512);
        let coverage_growth = tables > old_tables;
        // Preflight descriptor space and physical alignment before a CID epoch.
        let mut descriptor = vec![0; descriptor_length as usize];
        self.raw.read_exact_at(descriptor_offset, &mut descriptor)?;
        resize_descriptor(&descriptor, new_size / 512)?;
        if !self.raw.len().is_multiple_of(65536) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VMDK resize requires grain-aligned physical length",
            ));
        }
        let gd_bytes = (tables * 4).div_ceil(512) * 512;
        let gt_bytes = tables * 2048;
        let metadata_bytes = (gd_bytes + gt_bytes) * if old_rgd == 0 { 1 } else { 2 };
        let new_overhead = if coverage_growth {
            (old_overhead + metadata_bytes).div_ceil(65536) * 65536
        } else {
            old_overhead
        };
        if new_overhead
            .checked_add(65536)
            .is_none_or(|length| length > 33 * 1024 * 1024 * 1024)
        {
            return Err(io::ErrorKind::Unsupported.into());
        }
        self.raw.require_single_link_for_journal()?;
        let mut transaction_index = 0;
        let mut state = self.operation()?;
        self.begin_mutation(&mut state)?;
        // Clear hidden suffix on growth too: arbitrary existing padding must
        // never become newly addressable guest bytes.
        let boundary = new_size.min(self.size);
        if !boundary.is_multiple_of(65536) {
            let index = (boundary / 65536) as usize;
            let physical = state.mappings[index];
            if physical != 0 {
                let mut payload = vec![0; 65536];
                self.raw.read_exact_at(physical, &mut payload)?;
                payload[boundary as usize % 65536..].fill(0);
                let patch = self.resize_patch(physical, payload, 0)?;
                self.resize_commit(
                    vec![patch],
                    self.raw.len(),
                    &mut state,
                    &mut transaction_index,
                    cut,
                )?;
            }
        }
        // Removed grains become zero in the old-capacity view as each bounded
        // transaction completes. Physical payload remains unreferenced.
        if count < state.mappings.len() {
            let mut index = count;
            while index < state.mappings.len() {
                let end = ((index / 512 + 1) * 512).min(state.mappings.len());
                if state.mappings[index..end]
                    .iter()
                    .all(|mapping| *mapping == 0)
                    && state.zero_mask[index..end].iter().all(|mask| !mask)
                {
                    index = end;
                    continue;
                }
                let mut patches =
                    vec![self.resize_patch(self.primary[index], vec![0; (end - index) * 4], 0)?];
                if let Some(offset) = self.redundant[index] {
                    patches.push(self.resize_patch(offset, vec![0; (end - index) * 4], 1)?);
                }
                self.resize_commit(
                    patches,
                    self.raw.len(),
                    &mut state,
                    &mut transaction_index,
                    cut,
                )?;
                state.mappings[index..end].fill(0);
                state.zero_mask[index..end].fill(false);
                index = end;
            }
        }
        if coverage_growth {
            // Only grains intersecting the new protected arena need relocation.
            for index in 0..state.mappings.len() {
                let physical = state.mappings[index];
                if physical == 0 || physical >= new_overhead {
                    continue;
                }
                let destination = self.raw.len().max(new_overhead).div_ceil(65536) * 65536;
                let mut payload = vec![0; 65536];
                self.raw.read_exact_at(physical, &mut payload)?;
                let entry = u32::try_from(destination / 512)
                    .map_err(|_| io::ErrorKind::Unsupported)?
                    .to_le_bytes()
                    .to_vec();
                let mut patches = vec![
                    self.resize_patch(destination, payload, 0)?,
                    self.resize_patch(self.primary[index], entry.clone(), 1)?,
                ];
                if let Some(offset) = self.redundant[index] {
                    patches.push(self.resize_patch(offset, entry, 2)?);
                }
                self.resize_commit(
                    patches,
                    destination + 65536,
                    &mut state,
                    &mut transaction_index,
                    cut,
                )?;
                state.mappings[index] = destination;
            }
        }
        let gd = if coverage_growth {
            old_overhead
        } else {
            old_gd
        };
        let rgd = if coverage_growth && old_rgd != 0 {
            gd + gd_bytes + gt_bytes
        } else {
            old_rgd
        };
        if coverage_growth {
            let mut metadata = vec![0; metadata_bytes as usize];
            for (directory, start) in [(gd, 0usize), (rgd, (gd_bytes + gt_bytes) as usize)] {
                if directory == 0 {
                    continue;
                }
                let gt = directory + gd_bytes;
                for table in 0..tables as usize {
                    let entry = u32::try_from((gt + table as u64 * 2048) / 512)
                        .map_err(|_| io::ErrorKind::Unsupported)?;
                    metadata[start + table * 4..start + table * 4 + 4]
                        .copy_from_slice(&entry.to_le_bytes());
                }
                for (index, physical) in state.mappings.iter().enumerate() {
                    let entry =
                        u32::try_from(*physical / 512).map_err(|_| io::ErrorKind::Unsupported)?;
                    let at = start + gd_bytes as usize + index * 4;
                    metadata[at..at + 4].copy_from_slice(&entry.to_le_bytes());
                }
            }
            for (page, bytes) in metadata.chunks(1048576).enumerate() {
                let offset = gd + page as u64 * 1048576;
                let patch = self.resize_patch(offset, bytes.to_vec(), 0)?;
                let final_length = self.raw.len().max(new_overhead);
                self.resize_commit(
                    vec![patch],
                    final_length,
                    &mut state,
                    &mut transaction_index,
                    cut,
                )?;
            }
        }
        // CID may have changed during begin_mutation. Re-read rather than
        // publishing the preflight descriptor's previous epoch.
        self.raw.read_exact_at(descriptor_offset, &mut descriptor)?;
        let descriptor = resize_descriptor(&descriptor, new_size / 512)?;
        let cid_offset = descriptor_cid_offset(&descriptor, descriptor_offset)?;
        header[12..20].copy_from_slice(&(new_size / 512).to_le_bytes());
        header[48..56].copy_from_slice(&(rgd / 512).to_le_bytes());
        header[56..64].copy_from_slice(&(gd / 512).to_le_bytes());
        header[64..72].copy_from_slice(&(new_overhead / 512).to_le_bytes());
        let patches = vec![
            self.resize_patch(0, header, 1)?,
            self.resize_patch(descriptor_offset, descriptor, 0)?,
        ];
        // Build all fallible cache state before capacity publication. The
        // transaction validates the complete old and proposed container views.
        let mut mappings = state.mappings.clone();
        mappings.resize(count, 0);
        let mut zero_mask = if coverage_growth {
            vec![false; count]
        } else {
            state.zero_mask.clone()
        };
        zero_mask.resize(count, false);
        let mut primary = Vec::with_capacity(count);
        let mut redundant = Vec::with_capacity(count);
        for index in 0..count {
            let mut entry = [0; 4];
            self.raw
                .read_exact_at(gd + index as u64 / 512 * 4, &mut entry)?;
            primary.push(u64::from(u32::from_le_bytes(entry)) * 512 + index as u64 % 512 * 4);
            if rgd == 0 {
                redundant.push(None);
            } else {
                self.raw
                    .read_exact_at(rgd + index as u64 / 512 * 4, &mut entry)?;
                redundant.push(Some(
                    u64::from(u32::from_le_bytes(entry)) * 512 + index as u64 % 512 * 4,
                ));
            }
        }
        self.resize_commit(
            patches,
            self.raw.len(),
            &mut state,
            &mut transaction_index,
            cut,
        )?;
        state.mappings = mappings;
        state.zero_mask = zero_mask;
        drop(state);
        self.size = new_size;
        self.cid_offset = cid_offset;
        self.primary = primary;
        self.redundant = redundant;
        Ok(())
    }

    fn resize_patch(
        &self,
        offset: u64,
        new: Vec<u8>,
        order: u32,
    ) -> io::Result<crate::transaction::Patch> {
        let mut old = vec![0; self.raw.len().saturating_sub(offset).min(new.len() as u64) as usize];
        self.raw.read_exact_at(offset, &mut old)?;
        Ok(crate::transaction::Patch {
            offset,
            old,
            new,
            order,
        })
    }

    fn resize_commit(
        &self,
        mut patches: Vec<crate::transaction::Patch>,
        final_length: u64,
        state: &mut State,
        index: &mut usize,
        cut: Option<(usize, usize)>,
    ) -> io::Result<()> {
        let original_length = self.raw.len();
        let original_digest = crate::transaction::digest_reader(&LockedSource {
            raw: self.raw.clone(),
            size: original_length,
        })?;
        patches.sort_unstable_by_key(|patch| patch.offset);
        let record = crate::transaction::Record {
            original_length,
            final_length,
            original_digest,
            patches,
        };
        let stage = cut
            .filter(|(transaction, _)| *transaction == *index)
            .map(|(_, stage)| stage);
        *index += 1;
        if let Err(error) =
            crate::transaction::commit(&self.path, self.raw.clone(), record, stage, &|source| {
                Vmdk::open_parented(source, self.parent.clone()).map(drop)
            })
        {
            state.failed = true;
            return Err(error);
        }
        Ok(())
    }
    /// Open an existing standalone flat or Linux split hosted sparse descriptor.
    /// Every authorized extent and the descriptor retain locks and opened identities. Parsing is bounded
    /// to 64 KiB and authorization to 256 paths; capacity is at most 32 GiB. A fresh CID
    /// is synced before payload mutation. Writes may partially complete on I/O failure;
    /// at most 256 extents (split extents at most 2 GiB). Cross-extent I/O can complete a
    /// prefix before failure. Linux `twoGbMaxExtentSparse` supports allocated-grain
    /// overwrites, zeroing and existing-table grain allocation through a complete-set
    /// recovery coordinator. Allocation requires grain-aligned physical tails and
    /// bounded whole-call projected sizes/budgets. Missing tables use a bounded appended
    /// arena in empty extents or validated metadata padding in allocated extents. Backed
    /// split writes, native descriptor discard, creation and resizing are unsupported.
    pub fn open_descriptor(
        path: impl AsRef<Path>,
        authorized_extent_paths: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        Self::open_descriptor_policy(
            path.as_ref(),
            authorized_extent_paths,
            crate::RecoveryPolicy::Recover,
        )
    }
    fn open_descriptor_policy(
        path: &std::path::Path,
        authorized_extent_paths: &[std::path::PathBuf],
        policy: crate::RecoveryPolicy,
    ) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        if sparse_set::matches_profile(path)? {
            let opened = sparse_set::open_policy(path, authorized_extent_paths, false, policy)?;
            return Self::from_sparse(opened);
        }
        let opened = flat::open_policy(path, authorized_extent_paths, policy)?;
        Ok(Self {
            raw: opened.descriptor,
            _identity: opened.identity,
            path: opened.path,
            primary: Vec::new(),
            redundant: Vec::new(),
            cid_offset: opened.cid_offset,
            cid_width: opened.cid_width,
            size: opened.length,
            flat: Some(opened.flat),
            #[cfg(target_os = "linux")]
            sparse: None,
            parent: None,
            operation: Mutex::new(State {
                mappings: Vec::new(),
                zero_mask: Vec::new(),
                epoch: false,
                failed: false,
            }),
        })
    }
    #[cfg(target_os = "linux")]
    fn from_sparse(opened: sparse_set::Opened) -> io::Result<Self> {
        let size = opened.sparse.len();
        let parent = opened.sparse.parent_reader();
        Ok(Self {
            raw: opened.raw,
            _identity: opened.identity,
            path: opened.path,
            primary: Vec::new(),
            redundant: Vec::new(),
            cid_offset: 0,
            cid_width: 0,
            size,
            flat: None,
            sparse: Some(Box::new(opened.sparse)),
            parent,
            operation: Mutex::new(State {
                mappings: Vec::new(),
                zero_mask: Vec::new(),
                epoch: false,
                failed: false,
            }),
        })
    }
    /// Whether this opened writer uses an explicitly authorized external descriptor.
    pub fn is_descriptor(&self) -> bool {
        self.flat.is_some() || self.has_sparse_descriptor()
    }
    fn has_sparse_descriptor(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.sparse.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
    pub(crate) fn info_profile(&self) -> (Option<u64>, bool) {
        (
            if self.is_descriptor() {
                None
            } else {
                Some(65536)
            },
            self.is_descriptor(),
        )
    }

    /// Create a sparse hosted overlay over an explicitly authorized immutable parent.
    /// Ancestors and descriptor extent files also require explicit authorization.
    /// Linux journal recovery is required; existing output paths are never overwritten.
    pub fn create_overlay(
        path: impl AsRef<Path>,
        parent_path: impl AsRef<Path>,
        authorized_paths: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let parent_path = parent_path.as_ref().canonicalize()?;
        let mut allowed = false;
        for name in authorized_paths {
            if name.canonicalize()? == parent_path {
                allowed = true;
            }
        }
        if !allowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "VMDK parent requires explicit authorization",
            ));
        }
        let parent = Arc::new(Vmdk::open_chain(&parent_path, authorized_paths)?);
        Self::check_size(parent.len())?;
        let hint = parent_path.to_str().ok_or(io::ErrorKind::InvalidInput)?;
        struct Zero(u64);
        impl ReadAt for Zero {
            fn len(&self) -> u64 {
                self.0
            }
            fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
                crate::check_range(offset, dst.len() as u64, self.0)?;
                dst.fill(0);
                Ok(())
            }
        }
        let file = crate::vmdk_write::create_locked_vmdk_with_parent(
            path.as_ref(),
            &Zero(parent.len()),
            false,
            Some((parent.content_id().ok_or(io::ErrorKind::InvalidData)?, hint)),
        )?;
        Self::from_raw_parent(
            RawWriter::from_locked_file(file)?,
            path.as_ref().canonicalize()?,
            Some(parent),
            None,
        )
    }
    /// Lock a monolithic or split hosted sparse child and resolve its explicitly authorized immutable parents.
    pub fn open_chain(
        path: impl AsRef<Path>,
        authorized_paths: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::ErrorKind::Unsupported.into());
        }
        #[cfg(target_os = "linux")]
        if sparse_set::matches_profile(path.as_ref())? {
            return Self::from_sparse(sparse_set::open_chain(path.as_ref(), authorized_paths)?);
        }
        let raw = RawWriter::open(path.as_ref())?;
        raw.require_single_link_for_journal()?;
        let path = path.as_ref().canonicalize()?;
        let raw = Arc::new(raw);
        let source = Arc::new(LockedSource {
            size: raw.len(),
            raw: raw.clone(),
        });
        let (parent, identity) =
            Vmdk::resolve_writer_parent(source, &path, authorized_paths, raw.opened_identity()?)?;
        let raw =
            Arc::try_unwrap(raw).map_err(|_| io::Error::other("VMDK source still borrowed"))?;
        Self::from_raw_parent(raw, path, parent, Some(identity))
    }
    /// Whether writes use an authorized immutable parent for copy-on-write reads.
    pub fn has_parent(&self) -> bool {
        self.parent.is_some()
    }

    /// Create a fully allocated image while retaining its exclusive lock.
    ///
    /// Initial contents read as zero; source capacity is limited to 32 GiB for
    /// bounded ownership validation. Existing paths are never overwritten.
    pub fn create(path: impl AsRef<Path>, size: u64) -> io::Result<Self> {
        Self::create_profile(path, size, false)
    }
    /// Create a writable sparse hosted image with preallocated grain tables.
    /// Linux sidecar recovery and exclusive access are required for allocation.
    pub fn create_sparse(path: impl AsRef<Path>, size: u64) -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VMDK sparse allocation requires Linux",
            ));
        }
        Self::create_profile(path, size, true)
    }
    fn create_profile(path: impl AsRef<Path>, size: u64, sparse: bool) -> io::Result<Self> {
        Self::check_size(size)?;
        let path = path.as_ref();
        struct Zero(u64);
        impl ReadAt for Zero {
            fn len(&self) -> u64 {
                self.0
            }
            fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
                crate::check_range(offset, dst.len() as u64, self.0)?;
                dst.fill(0);
                Ok(())
            }
        }
        let file = crate::vmdk_write::create_locked_vmdk(path, &Zero(size), !sparse)?;
        Self::from_raw(RawWriter::from_locked_file(file)?, path.canonicalize()?)
    }

    /// Lock, recover and validate an existing standalone hosted sparse image.
    ///
    /// Unsupported profiles and invalid ownership are rejected before mutation.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_raw(
            RawWriter::open(path.as_ref())?,
            path.as_ref().canonicalize()?,
        )
    }
    pub(crate) fn open_policy(
        path: &std::path::Path,
        authorized: Option<&[std::path::PathBuf]>,
        policy: crate::RecoveryPolicy,
    ) -> io::Result<Self> {
        if let Some(paths) = authorized {
            #[cfg(target_os = "linux")]
            if sparse_set::matches_profile(path)? {
                return Self::from_sparse(sparse_set::open_policy(path, paths, true, policy)?);
            }
            let source = crate::RawDisk::open(path)?;
            let mut magic = [0; 4];
            if source.len() >= 4 {
                source.read_exact_at(0, &mut magic)?;
            }
            drop(source);
            if magic != *b"KDMV" {
                return Self::open_descriptor_policy(path, paths, policy);
            }
            if !cfg!(target_os = "linux") {
                return Err(io::ErrorKind::Unsupported.into());
            }
            let raw = Arc::new(RawWriter::open(path)?);
            let path = path.canonicalize()?;
            let source = Arc::new(LockedSource {
                size: raw.len(),
                raw: raw.clone(),
            });
            let (parent, identity) =
                Vmdk::resolve_writer_parent(source, &path, paths, raw.opened_identity()?)?;
            let raw =
                Arc::try_unwrap(raw).map_err(|_| io::Error::other("VMDK source still borrowed"))?;
            Self::from_raw_parent_policy(raw, path, parent, Some(identity), policy)
        } else {
            Self::from_raw_parent_policy(
                RawWriter::open(path)?,
                path.canonicalize()?,
                None,
                None,
                policy,
            )
        }
    }

    fn check_size(size: u64) -> io::Result<()> {
        if size == 0 || size > 32 * 1024 * 1024 * 1024 || !size.is_multiple_of(512) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VMDK writer capacity must be sector-aligned and at most 32 GiB",
            ));
        }
        Ok(())
    }

    fn from_raw(raw: RawWriter, path: std::path::PathBuf) -> io::Result<Self> {
        Self::from_raw_parent(raw, path, None, None)
    }
    fn from_raw_parent(
        raw: RawWriter,
        path: std::path::PathBuf,
        parent: Option<Arc<Vmdk>>,
        identity: Option<same_file::Handle>,
    ) -> io::Result<Self> {
        Self::from_raw_parent_policy(raw, path, parent, identity, crate::RecoveryPolicy::Recover)
    }
    fn from_raw_parent_policy(
        raw: RawWriter,
        path: std::path::PathBuf,
        parent: Option<Arc<Vmdk>>,
        identity: Option<same_file::Handle>,
        policy: crate::RecoveryPolicy,
    ) -> io::Result<Self> {
        let identity = match identity {
            Some(identity) => identity,
            None => raw.opened_identity()?,
        };
        let raw = Arc::new(raw);
        #[cfg(target_os = "linux")]
        raw.require_single_link_for_journal()?;
        policy.check(crate::transaction::pending(&path)?)?;
        if policy == crate::RecoveryPolicy::Recover && crate::transaction::pending(&path)? {
            crate::transaction::recover(&path, raw.clone(), &|source| {
                Vmdk::open_parented(source, parent.clone()).map(drop)
            })?;
        }
        let source = Arc::new(LockedSource {
            size: raw.len(),
            raw: raw.clone(),
        });
        let disk = Vmdk::open_parented(source, parent.clone())?;
        let size = disk.len();
        Self::check_size(size)?;
        let mappings = disk.writer_mappings()?;
        let zero_mask = disk.writer_zero_mask();
        let mut header = [0; 512];
        raw.read_exact_at(0, &mut header)?;
        let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let value = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
        if word(44) != 512 {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let descriptor_offset = value(28) * 512;
        let descriptor_length = value(36) * 512;
        if descriptor_length == 0 || descriptor_length > 1048576 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VMDK writer requires bounded embedded descriptor",
            ));
        }
        let mut desc = vec![0; descriptor_length as usize];
        raw.read_exact_at(descriptor_offset, &mut desc)?;
        let end = desc.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        let text = std::str::from_utf8(&desc[..end]).map_err(|_| io::ErrorKind::InvalidData)?;
        let mut cid_offset = None;
        let mut cid_width = 0;
        let mut cursor = 0usize;
        for line in text.split_inclusive('\n') {
            if let Some((key, value)) = line.split_once('=')
                && key.trim() == "CID"
            {
                let value = value.trim();
                if cid_offset.is_some()
                    || (value.is_empty() || value.len() > 8)
                    || u32::from_str_radix(value, 16).is_err()
                {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                cid_width = value.len();
                let equals = line.find('=').unwrap();
                let whitespace = line[equals + 1..].len() - line[equals + 1..].trim_start().len();
                cid_offset = Some(descriptor_offset + (cursor + equals + 1 + whitespace) as u64);
            }
            cursor += line.len();
        }
        let cid_offset = cid_offset.ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "VMDK writer requires CID")
        })?;
        let mut primary = Vec::with_capacity(mappings.len());
        let mut redundant = Vec::with_capacity(mappings.len());
        for table in 0..(mappings.len() as u64).div_ceil(512) {
            let mut entry = [0; 4];
            raw.read_exact_at(value(56) * 512 + table * 4, &mut entry)?;
            let p = u64::from(u32::from_le_bytes(entry)) * 512;
            if p == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "VMDK writer requires existing grain tables",
                ));
            }
            let r = if value(48) != 0 {
                raw.read_exact_at(value(48) * 512 + table * 4, &mut entry)?;
                let r = u64::from(u32::from_le_bytes(entry)) * 512;
                if r == 0 {
                    return Err(io::ErrorKind::Unsupported.into());
                }
                Some(r)
            } else {
                None
            };
            for within in 0..512u64 {
                if table * 512 + within >= mappings.len() as u64 {
                    break;
                }
                primary.push(p + within * 4);
                redundant.push(r.map(|r| r + within * 4));
            }
        }
        Ok(Self {
            raw,
            _identity: identity,
            path,
            primary,
            redundant,
            cid_offset,
            cid_width,
            size,
            flat: None,
            #[cfg(target_os = "linux")]
            sparse: None,
            parent,
            operation: Mutex::new(State {
                mappings,
                zero_mask,
                epoch: false,
                failed: false,
            }),
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
            .map_err(|_| io::Error::other("VMDK writer mutex poisoned"))?;
        if state.failed {
            return Err(io::Error::other(
                "VMDK transaction failed; reopen for recovery",
            ));
        }
        Ok(state)
    }

    fn begin_mutation(&self, state: &mut State) -> io::Result<()> {
        if !state.epoch {
            let mut old = [0; 8];
            self.raw
                .read_exact_at(self.cid_offset, &mut old[..self.cid_width])?;
            let old_cid = u32::from_str_radix(
                std::str::from_utf8(&old[..self.cid_width])
                    .map_err(|_| io::ErrorKind::InvalidData)?,
                16,
            )
            .map_err(|_| io::ErrorKind::InvalidData)?;
            let mut chosen = None;
            for _ in 0..4 {
                let mut bytes = [0; 4];
                getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
                let mask = if self.cid_width == 8 {
                    u32::MAX
                } else {
                    (1u32 << (self.cid_width * 4)) - 1
                };
                let cid = u32::from_le_bytes(bytes) & mask;
                if cid == u32::MAX || cid == old_cid {
                    continue;
                }
                let value = format!("{cid:0width$x}", width = self.cid_width);
                if value.as_bytes() != &old[..self.cid_width] {
                    chosen = Some(value);
                    break;
                }
            }
            let value =
                chosen.ok_or_else(|| io::Error::other("could not generate fresh VMDK CID"))?;
            if let Err(error) = self
                .raw
                .write_all_at(self.cid_offset, value.as_bytes())
                .and_then(|_| self.raw.flush())
            {
                state.failed = true;
                return Err(error);
            }
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
        let start = self.raw.len();
        if !start.is_multiple_of(65536) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VMDK physical tail must be grain aligned",
            ));
        }
        let entry = u32::try_from(start / 512).map_err(|_| io::ErrorKind::Unsupported)?;
        let digest = transaction::digest_reader(&LockedSource {
            raw: self.raw.clone(),
            size: start,
        })?;
        let mut payload = vec![0; 65536];
        if !state.zero_mask[index]
            && let Some(parent) = &self.parent
        {
            let offset = index as u64 * 65536;
            let count = (self.size - offset).min(65536) as usize;
            parent.read_exact_at(offset, &mut payload[..count])?;
        }
        payload[within as usize..within as usize + bytes.len()].copy_from_slice(bytes);
        let mut patches = Vec::new();
        let mut old = vec![0; 4];
        self.raw.read_exact_at(self.primary[index], &mut old)?;
        patches.push(Patch {
            order: if self.redundant[index].is_some() {
                2
            } else {
                1
            },
            offset: self.primary[index],
            old,
            new: entry.to_le_bytes().to_vec(),
        });
        if let Some(offset) = self.redundant[index] {
            let mut old = vec![0; 4];
            self.raw.read_exact_at(offset, &mut old)?;
            patches.push(Patch {
                order: 1,
                offset,
                old,
                new: entry.to_le_bytes().to_vec(),
            });
        }
        patches.push(Patch {
            order: 0,
            offset: start,
            old: vec![],
            new: payload,
        });
        patches.sort_unstable_by_key(|patch| patch.offset);
        let record = Record {
            original_length: start,
            final_length: start + 65536,
            original_digest: digest,
            patches,
        };
        if let Err(error) =
            transaction::commit(&self.path, self.raw.clone(), record, cut, &|source| {
                Vmdk::open_parented(source, self.parent.clone()).map(drop)
            })
        {
            state.failed = true;
            return Err(error);
        }
        state.mappings[index] = start;
        state.zero_mask[index] = false;
        Ok(())
    }

    /// Write a bounded range, allocating unallocated grains through the journal.
    pub fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(sparse) = &self.sparse {
            return sparse.write_all_at(offset, data);
        }
        crate::check_range(offset, data.len() as u64, self.size)?;
        let mut state = self.operation()?;
        if let Some(flat) = &self.flat {
            if !data.is_empty() {
                self.begin_mutation(&mut state)?;
            }
            return flat.write_all_at(offset, data);
        }
        if !data.is_empty() {
            let first = offset / 65536;
            let last = (offset + data.len() as u64 - 1) / 65536;
            if state.mappings[first as usize..=last as usize].contains(&0) {
                self.raw.require_single_link_for_journal()?;
                if !self.raw.len().is_multiple_of(65536) {
                    return Err(io::ErrorKind::Unsupported.into());
                }
            }
            self.begin_mutation(&mut state)?;
        }
        let mut done = 0;
        while done < data.len() {
            let position = offset + done as u64;
            let count = (65536 - position % 65536).min((data.len() - done) as u64) as usize;
            let index = (position / 65536) as usize;
            if state.mappings[index] == 0 {
                self.allocate(
                    index,
                    position % 65536,
                    &data[done..done + count],
                    &mut state,
                    None,
                )?;
            }
            self.raw.write_all_at(
                state.mappings[index] + position % 65536,
                &data[done..done + count],
            )?;
            done += count;
        }
        Ok(())
    }

    /// Read a bounded logical range from the currently written image.
    pub fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(sparse) = &self.sparse {
            return sparse.read_exact_at(offset, dst);
        }
        crate::check_range(offset, dst.len() as u64, self.size)?;
        let state = self.operation()?;
        if let Some(flat) = &self.flat {
            return flat.read_exact_at(offset, dst);
        }
        let mut done = 0;
        while done < dst.len() {
            let position = offset + done as u64;
            let count = (65536 - position % 65536).min((dst.len() - done) as u64) as usize;
            let mapping = state.mappings[(position / 65536) as usize];
            if mapping == 0
                && !state.zero_mask[(position / 65536) as usize]
                && let Some(parent) = &self.parent
            {
                parent.read_exact_at(position, &mut dst[done..done + count])?;
            } else if mapping == 0 {
                dst[done..done + count].fill(0);
            } else {
                self.raw
                    .read_exact_at(mapping + position % 65536, &mut dst[done..done + count])?;
            }
            done += count;
        }
        Ok(())
    }

    /// Zero a bounded logical range while retaining its allocation.
    pub fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(sparse) = &self.sparse {
            return sparse.write_zeroes(offset, length);
        }
        crate::check_range(offset, length, self.size)?;
        let mut state = self.operation()?;
        if let Some(flat) = &self.flat {
            if length != 0 {
                self.begin_mutation(&mut state)?;
            }
            return flat.write_zeroes(offset, length);
        }
        if length != 0 {
            let first = offset / 65536;
            let last = (offset + length - 1) / 65536;
            if self.parent.is_some() && state.mappings[first as usize..=last as usize].contains(&0)
            {
                self.raw.require_single_link_for_journal()?;
                if !self.raw.len().is_multiple_of(65536) {
                    return Err(io::ErrorKind::Unsupported.into());
                }
            }
            self.begin_mutation(&mut state)?;
        }
        let mut done = 0;
        while done < length {
            let position = offset + done;
            let count = (65536 - position % 65536).min(length - done);
            let index = (position / 65536) as usize;
            let mapping = state.mappings[index];
            if mapping == 0 && self.parent.is_some() && !state.zero_mask[index] {
                self.allocate(
                    index,
                    position % 65536,
                    &vec![0; count as usize],
                    &mut state,
                    None,
                )?;
            } else if mapping != 0 {
                self.raw.write_zeroes(mapping + position % 65536, count)?;
            }
            done += count;
        }
        Ok(())
    }

    /// Durably flush completed payload writes through the host filesystem.
    pub fn flush(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(sparse) = &self.sparse {
            return sparse.flush();
        }
        let mut state = self.operation()?;
        if let Some(flat) = &self.flat {
            flat.flush()?;
        }
        self.raw.flush()?;
        state.epoch = false;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod recovery_tests {
    use super::*;
    #[test]
    fn discard_zero_flag_and_mapping_publication_recover_at_every_cut() {
        let _process_boundary = crate::test_sync::writer_test();
        for overlay in [false, true] {
            for redundant in [false, true] {
                for zero_feature in [false, true] {
                    for allocated in [false, true] {
                        let patches = 1 + usize::from(redundant) + usize::from(!zero_feature);
                        for cut in 0..=5 + patches {
                            let directory = tempfile::tempdir().unwrap();
                            let parent = directory.path().join("parent.vmdk");
                            let base = VmdkWriter::create(&parent, 66048).unwrap();
                            base.write_all_at(0, &vec![53; 66048]).unwrap();
                            base.flush().unwrap();
                            drop(base);
                            let original_parent = std::fs::read(&parent).unwrap();
                            let path = directory.path().join("disk.vmdk");
                            let writer = if overlay {
                                VmdkWriter::create_overlay(
                                    &path,
                                    &parent,
                                    std::slice::from_ref(&parent),
                                )
                                .unwrap()
                            } else {
                                VmdkWriter::create_sparse(&path, 66048).unwrap()
                            };
                            if allocated {
                                writer.write_all_at(0, &[19; 512]).unwrap();
                            }
                            writer.flush().unwrap();
                            drop(writer);
                            let mut bytes = std::fs::read(&path).unwrap();
                            bytes[8..12].copy_from_slice(
                                &(1u32
                                    | if redundant { 2 } else { 0 }
                                    | if zero_feature { 4 } else { 0 })
                                .to_le_bytes(),
                            );
                            if redundant {
                                let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap())
                                    as usize
                                    * 512;
                                let gt = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap())
                                    as usize
                                    * 512;
                                let table = bytes[gt..gt + 2048].to_vec();
                                bytes[48..56].copy_from_slice(&30u64.to_le_bytes());
                                bytes[30 * 512..30 * 512 + 4].copy_from_slice(&31u32.to_le_bytes());
                                bytes[31 * 512..31 * 512 + 2048].copy_from_slice(&table);
                            }
                            std::fs::write(&path, bytes).unwrap();
                            let open = || {
                                if overlay {
                                    VmdkWriter::open_chain(&path, std::slice::from_ref(&parent))
                                } else {
                                    VmdkWriter::open(&path)
                                }
                            };
                            let writer = open().unwrap();
                            {
                                let mut state = writer.operation().unwrap();
                                writer.begin_mutation(&mut state).unwrap();
                                assert_eq!(
                                    writer
                                        .discard_grain(0, &mut state, Some(cut))
                                        .unwrap_err()
                                        .kind(),
                                    io::ErrorKind::Interrupted
                                );
                            }
                            assert!(writer.read_exact_at(0, &mut [0]).is_err());
                            assert!(
                                Vmdk::open_chain(
                                    &path,
                                    if overlay {
                                        std::slice::from_ref(&parent)
                                    } else {
                                        &[]
                                    }
                                )
                                .is_err()
                            );
                            drop(writer);
                            let writer = open().unwrap();
                            let mut actual = vec![1; 66048];
                            writer.read_exact_at(0, &mut actual).unwrap();
                            assert!(actual[..65536].iter().all(|byte| *byte == 0));
                            assert!(
                                actual[65536..]
                                    .iter()
                                    .all(|byte| *byte == if overlay { 53 } else { 0 })
                            );
                            writer.write_all_at(17, &[71; 32]).unwrap();
                            writer.read_exact_at(0, &mut actual).unwrap();
                            assert_eq!(&actual[17..49], &[71; 32]);
                            assert!(
                                actual[..17]
                                    .iter()
                                    .chain(actual[49..65536].iter())
                                    .all(|byte| *byte == 0)
                            );
                            drop(writer);
                            assert_eq!(std::fs::read(&parent).unwrap(), original_parent);
                        }
                    }
                }
            }
        }
    }
    #[test]
    #[ignore = "requires independent qemu-img oracle"]
    fn native_qemu_discard_redo_preserves_masked_parent_at_every_cut() {
        let _process_boundary = crate::test_sync::subprocess_test();
        for overlay in [false, true] {
            for cut in 0..=8 {
                let directory = tempfile::tempdir().unwrap();
                let parent = directory.path().join("parent.vmdk");
                let created = std::process::Command::new("qemu-img")
                    .args(["create", "-f", "vmdk"])
                    .arg(&parent)
                    .arg("131584")
                    .output()
                    .unwrap();
                assert!(created.status.success());
                let base = VmdkWriter::open(&parent).unwrap();
                base.write_all_at(0, &vec![53; 131584]).unwrap();
                base.flush().unwrap();
                drop(base);
                let original_parent = std::fs::read(&parent).unwrap();
                let path = directory.path().join("disk.vmdk");
                let mut command = std::process::Command::new("qemu-img");
                command.args(["create", "-f", "vmdk"]);
                if overlay {
                    command.args(["-F", "vmdk", "-b"]).arg(&parent);
                }
                let created = command.arg(&path).arg("131584").output().unwrap();
                assert!(created.status.success());
                let open = || {
                    if overlay {
                        VmdkWriter::open_chain(&path, std::slice::from_ref(&parent))
                    } else {
                        VmdkWriter::open(&path)
                    }
                };
                let writer = open().unwrap();
                writer.write_all_at(0, &[19; 512]).unwrap();
                {
                    let mut state = writer.operation().unwrap();
                    writer.begin_mutation(&mut state).unwrap();
                    assert_eq!(
                        writer
                            .discard_grain(0, &mut state, Some(cut))
                            .unwrap_err()
                            .kind(),
                        io::ErrorKind::Interrupted
                    );
                }
                drop(writer);
                let writer = open().unwrap();
                writer.write_all_at(17, &[71; 32]).unwrap();
                writer.flush().unwrap();
                drop(writer);
                let checked = std::process::Command::new("qemu-img")
                    .args(["check", "-f", "vmdk"])
                    .arg(&path)
                    .output()
                    .unwrap();
                assert!(
                    checked.status.success(),
                    "{}",
                    String::from_utf8_lossy(&checked.stderr)
                );
                let output = directory.path().join("converted.raw");
                let converted = std::process::Command::new("qemu-img")
                    .args(["convert", "-f", "vmdk", "-O", "raw"])
                    .arg(&path)
                    .arg(&output)
                    .output()
                    .unwrap();
                assert!(
                    converted.status.success(),
                    "{}",
                    String::from_utf8_lossy(&converted.stderr)
                );
                let mut expected = vec![0; 131584];
                expected[17..49].fill(71);
                if overlay {
                    expected[65536..].fill(53);
                }
                assert_eq!(std::fs::read(output).unwrap(), expected);
                assert_eq!(std::fs::read(parent).unwrap(), original_parent);
            }
        }
    }
    #[test]
    fn resize_transaction_cuts_recover_staged_growth_and_shrink() {
        let _process_boundary = crate::test_sync::writer_test();
        for shrinking in [false, true] {
            for transaction in 0..3 {
                let last_stage = if transaction == 2 || (!shrinking && transaction == 0) {
                    7
                } else {
                    6
                };
                for stage in 0..=last_stage {
                    let directory = tempfile::tempdir().unwrap();
                    let path = directory.path().join("disk.vmdk");
                    let mut writer = VmdkWriter::create_sparse(&path, 131072).unwrap();
                    writer.write_all_at(0, &[17; 32]).unwrap();
                    writer.write_all_at(65536, &[23; 32]).unwrap();
                    let new_size = if shrinking { 512 } else { 33554432 + 512 };
                    assert_eq!(
                        writer
                            .resize_inner(
                                new_size,
                                crate::ShrinkPolicy::AllowDataLoss,
                                Some((transaction, stage))
                            )
                            .unwrap_err()
                            .kind(),
                        io::ErrorKind::Interrupted
                    );
                    assert!(writer.read_exact_at(0, &mut [0; 1]).is_err());
                    drop(writer);
                    let mut writer = VmdkWriter::open(&path).unwrap();
                    assert_eq!(
                        writer.len(),
                        if transaction == 2 { new_size } else { 131072 }
                    );
                    let mut prefix = [0; 32];
                    writer.read_exact_at(0, &mut prefix).unwrap();
                    assert_eq!(prefix, [17; 32]);
                    if !shrinking {
                        writer.read_exact_at(65536, &mut prefix).unwrap();
                        assert_eq!(prefix, [23; 32]);
                    }
                    writer
                        .resize(new_size, crate::ShrinkPolicy::AllowDataLoss)
                        .unwrap();
                    writer.flush().unwrap();
                    drop(writer);
                    VmdkWriter::open(&path).unwrap();
                }
            }
        }
    }
    #[test]
    #[ignore = "requires independent qemu-img oracle"]
    fn resize_recovery_matches_qemu_at_every_staged_boundary() {
        use std::io::{Seek, SeekFrom, Write};
        let _process_boundary = crate::test_sync::subprocess_test();
        for shrinking in [false, true] {
            for transaction in 0..3 {
                let last_stage = if transaction == 2 || (shrinking && transaction == 1) {
                    7
                } else if !shrinking && transaction == 0 {
                    8
                } else {
                    6
                };
                for stage in 0..=last_stage {
                    let directory = tempfile::tempdir().unwrap();
                    let path = directory.path().join("disk.vmdk");
                    let created = std::process::Command::new("qemu-img")
                        .args(["create", "-f", "vmdk"])
                        .arg(&path)
                        .arg("131072")
                        .output()
                        .unwrap();
                    assert!(
                        created.status.success(),
                        "{}",
                        String::from_utf8_lossy(&created.stderr)
                    );
                    let mut writer = VmdkWriter::open(&path).unwrap();
                    writer.write_all_at(0, &[17; 32]).unwrap();
                    writer.write_all_at(65536, &[23; 32]).unwrap();
                    let new_size = if shrinking { 512 } else { 33554432 + 512 };
                    assert_eq!(
                        writer
                            .resize_inner(
                                new_size,
                                crate::ShrinkPolicy::AllowDataLoss,
                                Some((transaction, stage))
                            )
                            .unwrap_err()
                            .kind(),
                        io::ErrorKind::Interrupted
                    );
                    drop(writer);
                    let writer = VmdkWriter::open(&path).unwrap();
                    let size = writer.len();
                    writer.flush().unwrap();
                    drop(writer);
                    let expected_path = directory.path().join("expected.raw");
                    let mut expected = std::fs::File::create(&expected_path).unwrap();
                    expected.set_len(size).unwrap();
                    expected.write_all(&[17; 32]).unwrap();
                    if !shrinking || transaction == 0 {
                        expected.seek(SeekFrom::Start(65536)).unwrap();
                        expected.write_all(&[23; 32]).unwrap();
                    }
                    drop(expected);
                    let result = std::process::Command::new("qemu-img")
                        .args(["compare", "-f", "vmdk", "-F", "raw"])
                        .arg(&path)
                        .arg(&expected_path)
                        .output()
                        .unwrap();
                    assert!(
                        result.status.success(),
                        "resize transaction {transaction} stage {stage}, shrink={shrinking}: {} {}",
                        String::from_utf8_lossy(&result.stdout),
                        String::from_utf8_lossy(&result.stderr)
                    );
                }
            }
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn sparse_transaction_cuts_recover_private_grains_and_pending_reads_fail() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in 0..=8 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.vmdk");
            drop(VmdkWriter::create_sparse(&path, 2 * 65536).unwrap());
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[8..12].copy_from_slice(&3u32.to_le_bytes());
            bytes[48..56].copy_from_slice(&30u64.to_le_bytes());
            bytes[30 * 512..30 * 512 + 4].copy_from_slice(&31u32.to_le_bytes());
            std::fs::write(&path, bytes).unwrap();
            let writer = VmdkWriter::open(&path).unwrap();
            {
                let mut state = writer.operation().unwrap();
                writer.begin_mutation(&mut state).unwrap();
                assert!(
                    writer
                        .allocate(1, 17, &[8; 19], &mut state, Some(cut))
                        .is_err()
                );
            }
            if cut == 7 {
                let mut p = [0; 4];
                let mut r = [0; 4];
                writer.raw.read_exact_at(writer.primary[1], &mut p).unwrap();
                writer
                    .raw
                    .read_exact_at(writer.redundant[1].unwrap(), &mut r)
                    .unwrap();
                assert_eq!(p, [0; 4]);
                assert_ne!(r, [0; 4]);
            }
            assert!(writer.read_exact_at(0, &mut [0; 1]).is_err());
            assert!(Vmdk::open(Arc::new(crate::RawDisk::open(&path).unwrap())).is_err());
            drop(writer);
            crate::writer_open::refuse_pending_open(&path, crate::ImageFormat::Vmdk, None);
            let options =
                crate::WriterOpenOptions::default().recovery_policy(crate::RecoveryPolicy::Recover);
            let writer =
                crate::ImageWriter::open_with_options(&path, crate::ImageFormat::Vmdk, &options)
                    .unwrap();
            let mut out = [1; 64];
            writer.read_exact_at(65536, &mut out).unwrap();
            assert_eq!(&out[..17], &[0; 17]);
            assert_eq!(&out[17..36], &[8; 19]);
            assert_eq!(&out[36..], &[0; 28]);
            drop(writer);
            VmdkWriter::open(&path).unwrap();
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn inherited_grain_recovery_cuts_preserve_surrounding_parent_bytes() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in 0..=8 {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().join("base.vmdk");
            let child = dir.path().join("child.vmdk");
            let parent = VmdkWriter::create(&base, 65536).unwrap();
            parent.write_all_at(0, &vec![35; 65536]).unwrap();
            parent.flush().unwrap();
            drop(parent);
            let original = std::fs::read(&base).unwrap();
            drop(VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base)).unwrap());
            let mut bytes = std::fs::read(&child).unwrap();
            bytes[8..12].copy_from_slice(&3u32.to_le_bytes());
            bytes[48..56].copy_from_slice(&30u64.to_le_bytes());
            bytes[30 * 512..30 * 512 + 4].copy_from_slice(&31u32.to_le_bytes());
            std::fs::write(&child, bytes).unwrap();
            let writer = VmdkWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
            {
                let mut state = writer.operation().unwrap();
                writer.begin_mutation(&mut state).unwrap();
                assert!(
                    writer
                        .allocate(0, 17, &[0; 19], &mut state, Some(cut))
                        .is_err()
                );
            }
            assert!(Vmdk::open_chain(&child, std::slice::from_ref(&base)).is_err());
            drop(writer);
            let writer = VmdkWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
            let mut out = [1; 64];
            writer.read_exact_at(0, &mut out).unwrap();
            assert_eq!(&out[..17], &[35; 17]);
            assert_eq!(&out[17..36], &[0; 19]);
            assert_eq!(&out[36..], &[35; 28]);
            drop(writer);
            VmdkWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
            assert_eq!(std::fs::read(&base).unwrap(), original);
        }
    }
}
