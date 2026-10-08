use crate::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::{Qcow2, RawWriter, ReadAt};

const COPIED: u64 = 1 << 63;
const MASK: u64 = 0x00ff_ffff_ffff_fe00;

/// Positional writer for sparse, fully allocated, or authorized QCOW2 v3 overlays.
///
/// Opening acquires an exclusive operating system lock and validates exact active
/// ownership and private active L1 allocation clusters before permitting writes.
/// Only 64 KiB clusters, 16-bit refcounts,
/// sector-aligned capacity and uncompressed data mappings are supported. Backing
/// chains require explicit authorization through [`Self::open_chain`]. Internal
/// disk-only snapshots are validated across all saved maps; feature flags are
/// rejected. Sparse allocation and shared-payload copy-on-write use bounded sidecar redo journals
/// on Linux. Other platforms support private existing payload writes only.
/// Image users must exclude external access during mutation and recovery; the
/// native dirty flag does not prevent external tools from reading or repairing.
/// Payload writes are non-atomic and may partially complete on I/O failure.
/// External programs must respect the image lock and keep readers immutable.
/// Interrupted allocation requires reopening this writer to recover its journal.
/// Native capacity changes require exclusive mutable access through [`Self::resize`].
/// Native disk-only snapshot creation is available through [`Self::create_snapshot`].
/// Native disk-only deletion and revert use [`Self::delete_snapshot`] and
/// [`Self::revert_snapshot`]. Host storage reclamation remains unsupported.
pub struct Qcow2Writer {
    raw: Arc<RawWriter>,
    mappings: Mutex<Vec<u64>>,
    context: WriteContext,
    size: u64,
    operation: Mutex<()>,
    recovery_required: std::sync::atomic::AtomicBool,
    native_snapshot_count: u32,
    snapshot_creation_profile: bool,
    snapshot_lifecycle_profile: bool,
}

struct WriteContext {
    path: std::path::PathBuf,
    authorized: Vec<std::path::PathBuf>,
    parent: Option<Arc<dyn ReadAt>>,
}

use crate::source::{LockedSource, ZeroSource};

impl Qcow2Writer {
    pub(crate) fn container_size(&self) -> u64 {
        self.raw.len()
    }

    /// Create a fully allocated image while retaining its exclusive lock.
    ///
    /// Initial contents read as zero; source capacity is limited to 32 GiB for
    /// bounded ownership validation. Existing paths are never overwritten.
    pub fn create(path: impl AsRef<Path>, size: u64) -> io::Result<Self> {
        Self::create_profile(path.as_ref(), size, false)
    }

    /// Create an unallocated standalone image while retaining its exclusive lock.
    /// Linux redo persistence is required for later allocation. Existing paths
    /// are never overwritten; initialization failures can leave a partial file.
    pub fn create_sparse(path: impl AsRef<Path>, size: u64) -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "QCOW2 sparse allocation requires Linux",
            ));
        }
        Self::create_profile(path.as_ref(), size, true)
    }

    fn create_profile(path: &Path, size: u64, sparse: bool) -> io::Result<Self> {
        Self::check_size(size)?;
        let file = if sparse {
            crate::qcow2_write::create_locked_sparse_qcow2(path, &ZeroSource(size))?
        } else {
            crate::qcow2_write::create_locked_qcow2(path, &ZeroSource(size))?
        };
        Self::from_raw(
            RawWriter::from_locked_file(file)?,
            path.canonicalize()?,
            &[],
        )
    }

    /// Lock, recover a pending journal, and validate an existing standalone image.
    ///
    /// Unsupported profiles and invalid ownership are rejected before mutation.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_raw(
            RawWriter::open(path.as_ref())?,
            path.as_ref().canonicalize()?,
            &[],
        )
    }

    /// Open a writable overlay with explicitly authorized immutable backing files.
    ///
    /// Retains the exclusive child handle and read-only parent chain. Embedded
    /// paths, chain depth, identities and parser budgets use the reader policy.
    /// Recovery of a backed child requires the same explicit authorization.
    pub fn open_chain(
        path: impl AsRef<Path>,
        authorized_backing_paths: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        Self::from_raw(
            RawWriter::open(path.as_ref())?,
            path.as_ref().canonicalize()?,
            authorized_backing_paths,
        )
    }

    fn check_size(size: u64) -> io::Result<()> {
        if size > 32 * 1024 * 1024 * 1024 || !size.is_multiple_of(512) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "QCOW2 writer capacity must be sector-aligned and at most 32 GiB",
            ));
        }
        Ok(())
    }

    fn from_raw(
        raw: RawWriter,
        path: std::path::PathBuf,
        authorized: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        Self::from_raw_policy(raw, path, authorized, crate::RecoveryPolicy::Recover)
    }

    pub(crate) fn open_policy(
        path: &std::path::Path,
        authorized: &[std::path::PathBuf],
        policy: crate::RecoveryPolicy,
    ) -> io::Result<Self> {
        Self::from_raw_policy(
            RawWriter::open(path)?,
            path.canonicalize()?,
            authorized,
            policy,
        )
    }

    fn from_raw_policy(
        raw: RawWriter,
        path: std::path::PathBuf,
        authorized: &[std::path::PathBuf],
        policy: crate::RecoveryPolicy,
    ) -> io::Result<Self> {
        let raw = Arc::new(raw);
        #[cfg(target_os = "linux")]
        raw.require_single_link_for_journal()?;
        policy.check_sidecar(&journal::sidecar(&path))?;
        if policy == crate::RecoveryPolicy::RejectPending {
            let mut dirty = [0];
            raw.read_exact_at(79, &mut dirty)?;
            policy.check(dirty[0] & 1 != 0)?;
        }
        if policy == crate::RecoveryPolicy::Recover {
            journal::recover_authorized(&path, raw.clone(), authorized)?;
        }
        let mut header = [0; 104];
        raw.read_exact_at(0, &mut header)?;
        let value =
            |offset: usize| u64::from_be_bytes(header[offset..offset + 8].try_into().unwrap());
        let word =
            |offset: usize| u32::from_be_bytes(header[offset..offset + 4].try_into().unwrap());
        let size = value(24);
        Self::check_size(size)?;
        if &header[..4] != b"QFI\xfb"
            || word(4) != 3
            || word(20) != 16
            || word(96) != 4
            || !matches!(word(100), 104 | 112)
            || word(32) != 0
            || value(72) != 0
            || value(80) != 0
            || value(88) != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported QCOW2 positional writer profile",
            ));
        }
        if word(100) == 112 {
            let mut extra = [0; 8];
            raw.read_exact_at(104, &mut extra)?;
            if extra != [0; 8] {
                return Err(io::ErrorKind::Unsupported.into());
            }
        }
        let source = Arc::new(LockedSource {
            size: raw.len(),
            raw: raw.clone(),
        });
        let disk = Qcow2::open_locked_chain(source, &path, authorized, raw.file_identity()?)?;
        disk.validate_active_mapping()?;
        // Existing allocation/COW transactions update the active L1 in place.
        // Read-only readers may accept shared L1 tables, but this writer must
        // own every allocation cluster, including unused final L1 entries.
        for cluster in 0..(u64::from(word(36)) * 8).div_ceil(65536) {
            let index = value(40) / 65536 + cluster;
            let mut block = [0; 8];
            raw.read_exact_at(value(48) + (index / 32768) * 8, &mut block)?;
            let mut counter = [0; 2];
            raw.read_exact_at(
                u64::from_be_bytes(block) + (index % 32768) * 2,
                &mut counter,
            )?;
            if u16::from_be_bytes(counter) != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "QCOW2 writer requires private active L1 allocation clusters",
                ));
            }
        }
        let mut mappings = Vec::with_capacity(size.div_ceil(65536) as usize);
        for guest in 0..size.div_ceil(65536) {
            let mut bytes = [0; 8];
            raw.read_exact_at(value(40) + (guest / 8192) * 8, &mut bytes)?;
            let l1 = u64::from_be_bytes(bytes);
            if l1 == 0 {
                mappings.push(0);
                continue;
            }
            if l1 & !(MASK | COPIED) != 0 {
                return Err(io::ErrorKind::Unsupported.into());
            }
            raw.read_exact_at((l1 & MASK) + (guest % 8192) * 8, &mut bytes)?;
            let l2 = u64::from_be_bytes(bytes);
            if l2 & !(MASK | COPIED | 1) != 0 {
                return Err(io::ErrorKind::Unsupported.into());
            }
            mappings.push(l2);
        }
        let snapshot_directory_length = disk.snapshot_directory()?.1;
        let snapshot_lifecycle_profile = raw.len() <= 33 * 1024 * 1024 * 1024
            && cfg!(target_os = "linux")
            && word(36) <= 8192
            && word(60) <= 64
            && snapshot_directory_length <= 65536;
        let snapshot_creation_profile = raw.len() <= 33 * 1024 * 1024 * 1024
            && cfg!(target_os = "linux")
            && word(36) <= 8192
            && word(60) < 64
            && snapshot_directory_length <= 65536 - 64;
        Ok(Self {
            raw,
            mappings: Mutex::new(mappings),
            context: WriteContext {
                path,
                authorized: authorized.to_vec(),
                parent: disk.backing_reader(),
            },
            size,
            operation: Mutex::new(()),
            recovery_required: std::sync::atomic::AtomicBool::new(false),
            native_snapshot_count: word(60),
            snapshot_creation_profile,
            snapshot_lifecycle_profile,
        })
    }

    pub(crate) fn native_snapshot_count(&self) -> u32 {
        self.native_snapshot_count
    }
    pub(crate) fn native_snapshot_lifecycle_supported(&self) -> bool {
        self.snapshot_lifecycle_profile
            && !self
                .recovery_required
                .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn native_snapshot_creation_supported(&self) -> bool {
        self.snapshot_creation_profile
            && self.native_snapshot_count < 64
            && !self
                .recovery_required
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn native_resize_supported(&self) -> bool {
        self.native_snapshot_lifecycle_supported()
    }

    /// Current virtual capacity in bytes.
    ///
    /// Capacity changes require exclusive mutable access through [`Self::resize`].
    pub fn len(&self) -> u64 {
        self.size
    }
    /// Change virtual capacity through one durable metadata transaction.
    ///
    /// Linux, sector alignment, a single-cluster L1 table and the 32 GiB writer
    /// limit are required. Growth reads zero. Shrink requires an explicit policy;
    /// removed mappings release container ownership, while a retained boundary
    /// cluster is copied and its invisible suffix cleared. No physical EOF
    /// shrinking is promised. Saved disk states retain their content and capacity
    /// through mapping COW.
    /// Authorized immutable parents are supported. Internal snapshot resources must fit
    /// the bounded lifecycle profile.
    /// The caller must exclude all external dependent snapshots and image access.
    /// A transaction above the bounded patch budget fails before mutation;
    /// interrupted mutation requires reopening for complete capacity recovery.
    pub fn resize(&mut self, new_size: u64, policy: crate::ShrinkPolicy) -> io::Result<()> {
        Self::check_size(new_size)?;
        let next = {
            let _guard = self.operation()?;
            if new_size == self.size {
                return Ok(());
            }
            let mut snapshot_count = [0; 4];
            self.raw.read_exact_at(60, &mut snapshot_count)?;
            if snapshot_count != [0; 4] && !self.native_snapshot_lifecycle_supported() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native resize snapshot profile exceeds lifecycle bounds",
                ));
            }
            let mappings = self
                .mappings
                .lock()
                .map_err(|_| io::Error::other("QCOW2 mapping mutex poisoned"))?;
            match allocator::resize(
                &self.context,
                self.raw.clone(),
                &mappings,
                self.size,
                new_size,
                policy,
                None,
            ) {
                Ok(next) => next,
                Err(error) => {
                    if journal_path(&self.context.path)
                        .try_exists()
                        .unwrap_or(true)
                    {
                        self.recovery_required
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    return Err(error);
                }
            }
        };
        *self
            .mappings
            .get_mut()
            .map_err(|_| io::Error::other("QCOW2 mapping mutex poisoned"))? = next;
        self.size = new_size;
        Ok(())
    }

    /// Whether this writer retains an explicitly authorized immutable parent.
    pub fn has_parent(&self) -> bool {
        self.context.parent.is_some()
    }
    /// Whether virtual capacity is zero.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    fn operation(&self) -> io::Result<std::sync::MutexGuard<'_, ()>> {
        let guard = self
            .operation
            .lock()
            .map_err(|_| io::Error::other("QCOW2 writer mutex poisoned"))?;
        if self
            .recovery_required
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(io::Error::other(
                "QCOW2 writer requires reopening for transaction recovery",
            ));
        }
        Ok(guard)
    }

    /// Write a bounded logical range, allocating or copying payload when needed.
    ///
    /// Allocation transactions are durably journaled. A failed transaction
    /// requires reopening this writer before further access.
    pub fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        crate::check_range(offset, data.len() as u64, self.size)?;
        let _guard = self.operation()?;
        let mut done = 0;
        while done < data.len() {
            let position = offset + done as u64;
            let count = (65536 - position % 65536).min((data.len() - done) as u64) as usize;
            let index = (position / 65536) as usize;
            let mut mappings = self
                .mappings
                .lock()
                .map_err(|_| io::Error::other("QCOW2 mapping mutex poisoned"))?;
            let descriptor = mappings[index];
            if descriptor & COPIED != 0 && descriptor & 1 == 0 && descriptor & MASK != 0 {
                self.raw.write_all_at(
                    (descriptor & MASK) + position % 65536,
                    &data[done..done + count],
                )?;
            } else {
                let mapped = self.allocate(
                    index as u64,
                    descriptor,
                    &mappings,
                    position % 65536,
                    &data[done..done + count],
                )?;
                mappings[index] = mapped;
            }
            done += count;
        }
        Ok(())
    }

    /// Read a bounded logical range from the currently written image.
    pub fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, dst.len() as u64, self.size)?;
        let _guard = self.operation()?;
        let mut done = 0;
        while done < dst.len() {
            let position = offset + done as u64;
            let count = (65536 - position % 65536).min((dst.len() - done) as u64) as usize;
            let mappings = self
                .mappings
                .lock()
                .map_err(|_| io::Error::other("QCOW2 mapping mutex poisoned"))?;
            let descriptor = mappings[(position / 65536) as usize];
            if descriptor & 1 != 0 {
                dst[done..done + count].fill(0);
            } else if descriptor & MASK == 0 {
                let part = &mut dst[done..done + count];
                part.fill(0);
                if let Some(parent) = &self.context.parent {
                    let available =
                        parent.len().saturating_sub(position).min(count as u64) as usize;
                    if available != 0 {
                        parent.read_exact_at(position, &mut part[..available])?;
                    }
                }
            } else {
                self.raw.read_exact_at(
                    (descriptor & MASK) + position % 65536,
                    &mut dst[done..done + count],
                )?;
            }
            done += count;
        }
        Ok(())
    }

    /// Zero a bounded logical range; shared payloads are copied when needed.
    pub fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        crate::check_range(offset, length, self.size)?;
        let _guard = self.operation()?;
        let mut done = 0;
        while done < length {
            let position = offset + done;
            let count = (65536 - position % 65536).min(length - done);
            let index = (position / 65536) as usize;
            let mut mappings = self
                .mappings
                .lock()
                .map_err(|_| io::Error::other("QCOW2 mapping mutex poisoned"))?;
            let descriptor = mappings[index];
            if descriptor & 1 == 0 && (descriptor & MASK != 0 || self.context.parent.is_some()) {
                if descriptor & COPIED != 0 && descriptor & MASK != 0 {
                    self.raw
                        .write_zeroes((descriptor & MASK) + position % 65536, count)?;
                } else {
                    let zeros = vec![0; count as usize];
                    mappings[index] = self.allocate(
                        index as u64,
                        descriptor,
                        &mappings,
                        position % 65536,
                        &zeros,
                    )?;
                }
            }
            done += count;
        }
        Ok(())
    }

    /// Discard complete 64 KiB guest clusters, guaranteeing subsequent zero reads.
    ///
    /// Nonempty offsets and lengths must be cluster-aligned; a final partial
    /// guest cluster is rejected with `InvalidInput` before mutation. Capacity
    /// does not change. Explicit zero mappings mask authorized backing content,
    /// and payload reference counts are released through durable transactions.
    /// Host hole punching or EOF truncation is not promised. Multiple clusters
    /// can partially complete on error; reopen to recover a failed transaction.
    /// Journaled metadata mutation currently requires Linux.
    pub fn discard(&self, offset: u64, length: u64) -> io::Result<()> {
        crate::check_range(offset, length, self.size)?;
        if length == 0 {
            return Ok(());
        }
        if !offset.is_multiple_of(65536) || !length.is_multiple_of(65536) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "QCOW2 native discard requires complete 64 KiB guest clusters",
            ));
        }
        let _guard = self.operation()?;
        let mut mappings = self
            .mappings
            .lock()
            .map_err(|_| io::Error::other("QCOW2 mapping mutex poisoned"))?;
        for guest in offset / 65536..(offset + length) / 65536 {
            let index = guest as usize;
            let descriptor = mappings[index];
            if descriptor == 1 {
                continue;
            }
            match allocator::discard(
                &self.context,
                self.raw.clone(),
                guest,
                descriptor,
                &mappings,
            ) {
                Ok(replacement) => mappings[index] = replacement,
                Err(error) => {
                    self.recovery_required
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn allocate(
        &self,
        guest: u64,
        descriptor: u64,
        mappings: &[u64],
        within: u64,
        data: &[u8],
    ) -> io::Result<u64> {
        match allocator::allocate(
            &self.context,
            self.raw.clone(),
            guest,
            descriptor,
            mappings,
            within,
            data,
        ) {
            Ok(mapping) => Ok(mapping),
            Err(error) => {
                self.recovery_required
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Err(error)
            }
        }
    }

    /// Durably flush completed payload writes through the host filesystem.
    pub fn flush(&self) -> io::Result<()> {
        let _guard = self.operation()?;
        self.raw.flush()
    }
}

#[path = "qcow2_journal.rs"]
mod journal;

#[path = "qcow2_allocate.rs"]
mod allocator;

pub(crate) fn journal_path(path: &Path) -> std::path::PathBuf {
    journal::sidecar(path)
}

#[path = "qcow2_snapshot_write.rs"]
mod snapshot_write;
