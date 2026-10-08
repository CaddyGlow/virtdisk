//! Bounded physical-container hashing, independent of logical payload quotas.
use crate::io;
#[cfg(feature = "std")]
use crate::io::{Read, Seek, SeekFrom};
use alloc::vec::Vec;
use core::fmt;
use sha2::{Digest, Sha256};
#[cfg(feature = "std")]
use std::fs::File;
const MAX_BYTES: u64 = 33 * 1024 * 1024 * 1024;
const MAX_CALLS: u64 = 1_048_576;
const MAX_SCRATCH: usize = 65_536;

/// Resource exhausted by physical-container validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PhysicalValidationResource {
    /// Requested physical read bytes, including failed attempts.
    PhysicalBytes,
    /// Attempted exact-read calls, including failed attempts.
    ReadCalls,
}
/// A physical validation preflight would exceed a cumulative caller ceiling.
#[derive(Debug)]
pub struct PhysicalValidationLimitExceeded {
    resource: PhysicalValidationResource,
    limit: u64,
    requested: u128,
}
impl PhysicalValidationLimitExceeded {
    /// Exhausted resource.
    pub fn resource(&self) -> PhysicalValidationResource {
        self.resource
    }
    /// Configured cumulative ceiling.
    pub fn limit(&self) -> u64 {
        self.limit
    }
    /// Requested total, including earlier accepted work.
    pub fn requested(&self) -> u128 {
        self.requested
    }
}
impl fmt::Display for PhysicalValidationLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "physical validation {:?} requires {}, limit {}",
            self.resource, self.requested, self.limit
        )
    }
}
impl core::error::Error for PhysicalValidationLimitExceeded {}

/// Validated ceilings for physical-container fingerprinting.
/// Defaults: 33 GiB cumulative physical bytes, 1,048,576 exact-read calls and
/// 64 KiB scratch. Callers may set each ceiling within these maxima; zero bytes/calls permit
/// empty files. These limits do not account metadata I/O or logical image data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalValidationLimits {
    bytes: u64,
    calls: u64,
    scratch: usize,
}
impl Default for PhysicalValidationLimits {
    fn default() -> Self {
        Self {
            bytes: MAX_BYTES,
            calls: MAX_CALLS,
            scratch: MAX_SCRATCH,
        }
    }
}
impl PhysicalValidationLimits {
    /// Set cumulative physical bytes to 0..=33 GiB.
    pub fn physical_bytes(mut self, bytes: u64) -> io::Result<Self> {
        if bytes > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "physical validation byte ceiling exceeds 33 GiB",
            ));
        }
        self.bytes = bytes;
        Ok(self)
    }
    /// Set cumulative exact-read calls to 0..=1,048,576.
    pub fn read_calls(mut self, calls: u64) -> io::Result<Self> {
        if calls > MAX_CALLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "physical validation read-call ceiling exceeds 1048576",
            ));
        }
        self.calls = calls;
        Ok(self)
    }
    /// Set one fallibly allocated scratch buffer to 1..=65536 bytes.
    pub fn scratch_bytes(mut self, bytes: usize) -> io::Result<Self> {
        if !(1..=MAX_SCRATCH).contains(&bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "physical validation scratch must be 1..=65536 bytes",
            ));
        }
        self.scratch = bytes;
        Ok(self)
    }
}
/// Cumulative physical work, retained across failures and repeated scans.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysicalValidationUsage {
    /// Requested physical read bytes, charged before each exact-read attempt.
    pub physical_bytes: u64,
    /// Physical bytes hashed in successfully completed chunks.
    pub hashed_bytes: u64,
    /// Exact-read calls attempted; individual system-call retries are unmeasured.
    pub read_calls: u64,
    /// Largest allocated scratch buffer, excluding allocator overhead.
    pub peak_scratch_bytes: u64,
}
/// Physical length and SHA-256 of one retained file's complete bytes.
/// This is not a logical disk hash, path authority or a standalone immutability
/// proof. Callers retain identity/ownership checks and exclude external mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalFingerprint {
    length: u64,
    digest: [u8; 32],
}
impl PhysicalFingerprint {
    /// Hashed physical file length in bytes.
    pub fn len(&self) -> u64 {
        self.length
    }
    /// Whether the hashed physical file is empty.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
    /// SHA-256 of the complete physical byte stream.
    pub fn sha256(&self) -> &[u8; 32] {
        &self.digest
    }
}
/// Reusable physical validation accounting, separate from `OperationContext`.
/// Whole-scan preflight refuses excessive work before data reads or allocation.
/// Metadata calls, seeking and system-call retries are outside accounting.
/// No callbacks run under the writer's retained mutex/file lock.
#[derive(Debug, Default)]
pub struct PhysicalValidationBudget {
    limits: PhysicalValidationLimits,
    usage: PhysicalValidationUsage,
}
impl PhysicalValidationBudget {
    /// Start a new budget with validated limits and zero cumulative usage.
    pub fn new(limits: PhysicalValidationLimits) -> Self {
        Self {
            limits,
            usage: PhysicalValidationUsage::default(),
        }
    }
    /// Accepted cumulative usage; inspecting it does not reset the budget.
    pub fn usage(&self) -> PhysicalValidationUsage {
        self.usage
    }
    fn preflight(&self, length: u64) -> io::Result<()> {
        let calls = length.div_ceil(self.limits.scratch as u64);
        for (resource, limit, requested) in [
            (
                PhysicalValidationResource::PhysicalBytes,
                self.limits.bytes,
                u128::from(self.usage.physical_bytes) + u128::from(length),
            ),
            (
                PhysicalValidationResource::ReadCalls,
                self.limits.calls,
                u128::from(self.usage.read_calls) + u128::from(calls),
            ),
        ] {
            if requested > u128::from(limit) {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceLimit,
                    PhysicalValidationLimitExceeded {
                        resource,
                        limit,
                        requested,
                    },
                ));
            }
        }
        Ok(())
    }
    /// Hash the complete bytes of supplied immutable storage with bounded scratch.
    /// This does not establish storage identity or protection against mutation.
    pub fn fingerprint_reader(
        &mut self,
        source: &dyn crate::ReadAt,
    ) -> io::Result<PhysicalFingerprint> {
        let length = source.len();
        self.preflight(length)?;
        let scratch = length.min(self.limits.scratch as u64) as usize;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(scratch)
            .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
        buffer.resize(scratch, 0);
        self.usage.peak_scratch_bytes = self.usage.peak_scratch_bytes.max(scratch as u64);
        let mut hash = Sha256::new();
        let mut done = 0;
        while done < length {
            let count = (length - done).min(buffer.len() as u64) as usize;
            self.usage.read_calls += 1;
            self.usage.physical_bytes += count as u64;
            source.read_exact_at(done, &mut buffer[..count])?;
            hash.update(&buffer[..count]);
            done += count as u64;
            self.usage.hashed_bytes += count as u64;
        }
        Ok(PhysicalFingerprint {
            length,
            digest: hash.finalize().into(),
        })
    }
    #[cfg(feature = "std")]
    pub(crate) fn fingerprint(
        &mut self,
        file: &mut File,
        length: u64,
    ) -> io::Result<PhysicalFingerprint> {
        self.preflight(length)?;
        if file.metadata()?.len() != length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "physical file length changed before validation",
            ));
        }
        let scratch = length.min(self.limits.scratch as u64) as usize;
        let mut buffer = crate::operation_context::scratch_buffer(scratch)?;
        self.usage.peak_scratch_bytes = self.usage.peak_scratch_bytes.max(scratch as u64);
        file.seek(SeekFrom::Start(0))?;
        let mut hash = Sha256::new();
        let mut done = 0;
        while done < length {
            let count = (length - done).min(buffer.len() as u64) as usize;
            self.usage.read_calls += 1;
            self.usage.physical_bytes += count as u64;
            file.read_exact(&mut buffer[..count])?;
            hash.update(&buffer[..count]);
            done += count as u64;
            self.usage.hashed_bytes += count as u64;
        }
        if file.metadata()?.len() != length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "physical file length changed during validation",
            ));
        }
        Ok(PhysicalFingerprint {
            length,
            digest: hash.finalize().into(),
        })
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    #[test]
    fn failed_read_preserves_attempt_and_scratch_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("write-only");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.set_len(512).unwrap();
        let limits = PhysicalValidationLimits::default()
            .scratch_bytes(8)
            .unwrap();
        let mut budget = PhysicalValidationBudget::new(limits);
        assert!(budget.fingerprint(&mut file, 512).is_err());
        assert_eq!(budget.usage().read_calls, 1);
        assert_eq!(budget.usage().physical_bytes, 8);
        assert_eq!(budget.usage().hashed_bytes, 0);
        assert_eq!(budget.usage().peak_scratch_bytes, 8);
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    struct Bytes;
    impl crate::ReadAt for Bytes {
        fn len(&self) -> u64 {
            3
        }
        fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
            crate::check_range(offset, bytes.len() as u64, 3)?;
            bytes.copy_from_slice(&b"abc"[offset as usize..offset as usize + bytes.len()]);
            Ok(())
        }
    }
    #[test]
    fn supplied_bytes_hash_with_cumulative_preflight() {
        let limits = PhysicalValidationLimits::default()
            .physical_bytes(3)
            .unwrap()
            .scratch_bytes(2)
            .unwrap();
        let mut budget = PhysicalValidationBudget::new(limits);
        let fingerprint = budget.fingerprint_reader(&Bytes).unwrap();
        assert_eq!(
            fingerprint.sha256(),
            &<[u8; 32]>::from(Sha256::digest(b"abc"))
        );
        assert_eq!(budget.usage().read_calls, 2);
        assert_eq!(budget.usage().physical_bytes, 3);
        assert_eq!(
            budget.fingerprint_reader(&Bytes).unwrap_err().kind(),
            io::ErrorKind::ResourceLimit
        );
        assert_eq!(budget.usage().read_calls, 2);
    }
}
