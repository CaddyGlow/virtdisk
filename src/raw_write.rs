use crate::io::{self, Read, Seek, SeekFrom, Write};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

/// A bounded, exclusively locked writable regular raw image.
///
/// Reads, writes, zeroing, resizing and flushing are serialized. The logical
/// capacity may change, so this type deliberately does not implement `ReadAt`.
/// The operating system lock excludes cooperating writers, including other
/// `RawWriter` handles; it cannot prevent non-cooperating external mutation on
/// systems with advisory locks. Keep other access to the image immutable.
/// Writes and zeroing may partially complete on I/O failure. Call [`Self::flush`]
/// to persist data and file metadata; creation does not sync the parent directory.
/// Zeroing writes bytes and does not promise physical space reclamation.
pub struct RawWriter {
    state: Mutex<State>,
}

struct State {
    file: File,
    length: u64,
}

impl RawWriter {
    /// Fingerprint complete physical bytes under the retained mutex and file lock.
    ///
    /// Uses a distinct cumulative physical budget, not logical image accounting.
    /// Whole-scan quota refusal precedes data reads; scratch is fallible and at
    /// most 64 KiB. Requested read bytes and exact-read attempts remain charged
    /// on error; completed hashed bytes are tracked separately. Capacity/content
    /// are not modified and no flush/callback occurs.
    /// Local writer operations cannot interleave with the scan. Callers exclude
    /// noncooperating external mutation and validate path identity separately;
    /// this digest does not grant authority or capture a guest filesystem state.
    pub fn physical_fingerprint(
        &self,
        budget: &mut crate::PhysicalValidationBudget,
    ) -> io::Result<crate::PhysicalFingerprint> {
        let mut state = self.state()?;
        let length = state.length;
        budget.fingerprint(&mut state.file, length)
    }

    /// Create a new zero-filled logical image without overwriting an existing path.
    ///
    /// Host filesystems may store the initial contents sparsely. A failed size
    /// initialization can leave the newly created file at the requested path.
    pub fn create(path: impl AsRef<Path>, size: u64) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        let writer = Self::from_file(file)?;
        writer.resize(size)?;
        Ok(writer)
    }

    /// Open an existing regular image and acquire a nonblocking exclusive lock.
    ///
    /// Fails when another cooperating handle has locked this image. Opening
    /// does not truncate the image or create a missing path.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_file(OpenOptions::new().read(true).write(true).open(path)?)
    }

    fn from_file(file: File) -> io::Result<Self> {
        file.try_lock().map_err(io::Error::from)?;
        Self::from_locked_file(file)
    }

    pub(crate) fn from_locked_file(file: File) -> io::Result<Self> {
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "raw image must be a regular file",
            ));
        }
        let length = file.metadata()?.len();
        Ok(Self {
            state: Mutex::new(State { file, length }),
        })
    }

    fn state(&self) -> io::Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("raw writer mutex poisoned"))
    }

    pub(crate) fn file_identity(&self) -> io::Result<Option<(u64, u64)>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = self.state()?.file.metadata()?;
            Ok(Some((metadata.dev(), metadata.ino())))
        }
        #[cfg(not(unix))]
        {
            Ok(None)
        }
    }
    pub(crate) fn opened_identity(&self) -> io::Result<same_file::Handle> {
        Ok(same_file::Handle::from_file(
            self.state()?.file.try_clone()?,
        )?)
    }

    pub(crate) fn require_single_link_for_journal(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            if self.state()?.file.metadata()?.nlink() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "journaled image must have exactly one filesystem hard link",
                ));
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "journaled allocation currently requires Linux",
            ))
        }
    }

    /// Current logical image length in bytes.
    pub fn len(&self) -> u64 {
        // No user callback runs while holding this mutex, so poisoning requires
        // an internal panic. Retain inspection of the last known capacity.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .length
    }

    /// Whether the image currently has zero logical capacity.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Write all bytes at a bounded logical offset, without extending the image.
    ///
    /// Invalid ranges are rejected before any bytes are changed. An I/O failure
    /// may leave a partially written range; writes are not atomic.
    pub fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        let mut state = self.state()?;
        crate::check_range(offset, data.len() as u64, state.length)?;
        state.file.seek(SeekFrom::Start(offset))?;
        Ok(state.file.write_all(data)?)
    }

    /// Read exactly a bounded range from the current logical image.
    ///
    /// An I/O failure may partially modify the destination.
    pub fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        let mut state = self.state()?;
        crate::check_range(offset, destination.len() as u64, state.length)?;
        state.file.seek(SeekFrom::Start(offset))?;
        Ok(state.file.read_exact(destination)?)
    }

    /// Write zero bytes to a bounded range using constant memory.
    ///
    /// This preserves zero-read semantics and does not punch filesystem holes.
    /// An I/O failure may leave the range partially zeroed.
    pub fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        let mut state = self.state()?;
        crate::check_range(offset, length, state.length)?;
        state.file.seek(SeekFrom::Start(offset))?;
        let zeros = [0; 64 * 1024];
        let mut remaining = length;
        while remaining != 0 {
            let count = remaining.min(zeros.len() as u64) as usize;
            state.file.write_all(&zeros[..count])?;
            remaining -= count as u64;
        }
        Ok(())
    }

    /// Change logical capacity; shrinking permanently discards the tail.
    ///
    /// New space reads as zero. This changes only file capacity, not partitions
    /// or filesystems. Callers must validate their layout before shrinking.
    pub fn resize(&self, new_size: u64) -> io::Result<()> {
        let mut state = self.state()?;
        state.file.set_len(new_size)?;
        state.length = new_size;
        Ok(())
    }

    /// Request host allocation for a checked existing range without changing bytes.
    ///
    /// Linux uses `fallocate(KEEP_SIZE)` under the writer's lock. Unsupported
    /// platforms/filesystems return `Unsupported`; no zero-write fallback is
    /// substituted. Zero-length valid ranges succeed without a host request.
    /// Existing data and capacity are preserved. Allocation granularity depends
    /// on the host filesystem; this does not unshare reflinked blocks or promise
    /// later COW allocation cannot fail. Failure may leave partial allocation.
    /// Call [`Self::flush`] to request durability.
    pub fn preallocate(&self, offset: u64, length: u64) -> io::Result<()> {
        let state = self.state()?;
        crate::check_range(offset, length, state.length)?;
        if length == 0 {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            if offset + length > i64::MAX as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "preallocation range exceeds host signed offset limit",
                ));
            }
            match rustix::fs::fallocate(
                &state.file,
                rustix::fs::FallocateFlags::KEEP_SIZE,
                offset,
                length,
            ) {
                Ok(()) => return Ok(()),
                Err(error)
                    if error == rustix::io::Errno::OPNOTSUPP
                        || error == rustix::io::Errno::NOSYS => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "host preallocation is unavailable",
        ))
    }

    /// Discard a checked range, preserving capacity and guaranteeing zero reads.
    ///
    /// Linux uses hole punching with `KEEP_SIZE`; partial host blocks are zeroed
    /// and complete host blocks may be released. Other platforms and unsupported
    /// filesystems require an explicit zero fallback. Real I/O errors propagate;
    /// they never silently trigger a fallback. Operations are serialized and
    /// durability requires [`Self::flush`]. Failure can leave partial effects.
    pub fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: crate::DiscardPolicy,
    ) -> io::Result<crate::DiscardResult> {
        let mut state = self.state()?;
        crate::check_range(offset, length, state.length)?;
        if length == 0 {
            return Ok(crate::DiscardResult::Zeroed);
        }
        #[cfg(target_os = "linux")]
        {
            use rustix::fs::{FallocateFlags, fallocate};
            if offset
                .checked_add(length)
                .is_none_or(|end| end > i64::MAX as u64)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "discard range exceeds host signed offset limit",
                ));
            }
            match fallocate(
                &state.file,
                FallocateFlags::PUNCH_HOLE | FallocateFlags::KEEP_SIZE,
                offset,
                length,
            ) {
                Ok(()) => return Ok(crate::DiscardResult::Deallocated),
                Err(error)
                    if error == rustix::io::Errno::OPNOTSUPP
                        || error == rustix::io::Errno::NOSYS => {}
                Err(error) => return Err(error.into()),
            }
        }
        if policy == crate::DiscardPolicy::RequireDeallocation {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "host hole punching is unavailable",
            ));
        }
        state.file.seek(SeekFrom::Start(offset))?;
        let zeros = [0; 65536];
        let mut remaining = length;
        while remaining != 0 {
            let count = remaining.min(zeros.len() as u64) as usize;
            state.file.write_all(&zeros[..count])?;
            remaining -= count as u64;
        }
        Ok(crate::DiscardResult::Zeroed)
    }

    /// Persist completed writes and file metadata through the host filesystem.
    ///
    /// Storage hardware and host filesystem determine the ultimate durability
    /// guarantee. This does not sync the image's parent directory.
    pub fn flush(&self) -> io::Result<()> {
        Ok(self.state()?.file.sync_all()?)
    }
}
