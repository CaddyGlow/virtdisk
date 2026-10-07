//! Explicit writable access, separate from immutable readers.
use crate::{Qcow2Writer, RawWriter, VdiWriter, VhdxWriter, VmdkWriter};
use std::io;

/// Whether discard must request deallocation or may fall back to writing zeroes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardPolicy {
    /// Return `Unsupported` if native deallocation is unavailable.
    RequireDeallocation,
    /// Permit bounded zero writes when native deallocation is unavailable.
    AllowZeroFallback,
}

/// How a completed discard produced its guaranteed zero-readable range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardResult {
    /// The host or container accepted deallocation. Reclamation granularity may
    /// leave some physical bytes allocated; container metadata release alone
    /// does not guarantee host file truncation or hole punching.
    Deallocated,
    /// The range was zeroed without promising allocation reclamation.
    Zeroed,
}

/// Bounded positional writes with explicit persistence.
///
/// Invalid ranges must be rejected before mutation. I/O failure can leave a
/// partially written range; arbitrary multi-sector writes are not atomic.
/// Callers must exclude resize and other management operations for the duration
/// of generic operations. Implementations serialize operations as documented.
pub trait WriteAt: Send + Sync {
    /// Current virtual capacity in bytes.
    fn len(&self) -> u64;
    /// Whether virtual capacity is zero.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Write all bytes to a checked range without implicitly extending capacity.
    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()>;
    /// Zero a checked range; does not imply deallocation or physical reclamation.
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        crate::check_range(offset, length, self.len())?;
        let zeros = [0; 64 * 1024];
        let mut done = 0;
        while done < length {
            let count = (length - done).min(zeros.len() as u64) as usize;
            self.write_all_at(offset + done, &zeros[..count])?;
            done += count as u64;
        }
        Ok(())
    }
    /// Persist completed writes and required metadata through the host.
    fn flush(&self) -> io::Result<()>;
    /// Discard a checked logical range while guaranteeing subsequent reads are zero.
    ///
    /// Does not change capacity. Alignment and recovery rules depend on the
    /// native profile. Errors may leave a partially completed range; flush
    /// completed operations explicitly. The default supports only the explicit
    /// zero fallback and rejects strict requests before mutation.
    fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
    ) -> io::Result<DiscardResult> {
        crate::check_range(offset, length, self.len())?;
        if length == 0 {
            return Ok(DiscardResult::Zeroed);
        }
        if policy == DiscardPolicy::RequireDeallocation {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native discard is unavailable",
            ));
        }
        self.write_zeroes(offset, length)?;
        Ok(DiscardResult::Zeroed)
    }
}

impl WriteAt for RawWriter {
    fn len(&self) -> u64 {
        Self::len(self)
    }
    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        Self::write_all_at(self, offset, data)
    }
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        Self::write_zeroes(self, offset, length)
    }
    fn flush(&self) -> io::Result<()> {
        Self::flush(self)
    }
    fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
    ) -> io::Result<DiscardResult> {
        Self::discard(self, offset, length, policy)
    }
}
impl WriteAt for Qcow2Writer {
    fn len(&self) -> u64 {
        Self::len(self)
    }
    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        Self::write_all_at(self, offset, data)
    }
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        Self::write_zeroes(self, offset, length)
    }
    fn flush(&self) -> io::Result<()> {
        Self::flush(self)
    }
    fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
    ) -> io::Result<DiscardResult> {
        crate::check_range(offset, length, self.len())?;
        if length == 0 {
            return Ok(DiscardResult::Zeroed);
        }
        if cfg!(target_os = "linux") && offset.is_multiple_of(65536) && length.is_multiple_of(65536)
        {
            Self::discard(self, offset, length)?;
            return Ok(DiscardResult::Deallocated);
        }
        if policy == DiscardPolicy::RequireDeallocation {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native QCOW2 discard requires Linux and complete 64 KiB clusters",
            ));
        }
        self.write_zeroes(offset, length)?;
        Ok(DiscardResult::Zeroed)
    }
}
impl WriteAt for VmdkWriter {
    fn len(&self) -> u64 {
        VmdkWriter::len(self)
    }
    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        VmdkWriter::write_all_at(self, offset, data)
    }
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        VmdkWriter::write_zeroes(self, offset, length)
    }
    fn flush(&self) -> io::Result<()> {
        VmdkWriter::flush(self)
    }
    fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
    ) -> io::Result<DiscardResult> {
        VmdkWriter::discard(self, offset, length, policy)
    }
}
impl WriteAt for VdiWriter {
    fn len(&self) -> u64 {
        VdiWriter::len(self)
    }
    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        VdiWriter::write_all_at(self, offset, data)
    }
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        VdiWriter::write_zeroes(self, offset, length)
    }
    fn flush(&self) -> io::Result<()> {
        VdiWriter::flush(self)
    }
    fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
    ) -> io::Result<DiscardResult> {
        VdiWriter::discard(self, offset, length, policy)
    }
}
impl WriteAt for VhdxWriter {
    fn len(&self) -> u64 {
        VhdxWriter::len(self)
    }
    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        VhdxWriter::write_all_at(self, offset, data)
    }
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        VhdxWriter::write_zeroes(self, offset, length)
    }
    fn flush(&self) -> io::Result<()> {
        VhdxWriter::flush(self)
    }
    fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
    ) -> io::Result<DiscardResult> {
        VhdxWriter::discard(self, offset, length, policy)
    }
}
