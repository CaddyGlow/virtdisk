//! Caller-controlled management work, progress and cancellation.
use crate::io;
use std::{fmt, ops::ControlFlow};

/// Resource measured by a common operation context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationResource {
    /// Completed logical bytes across operations using the context.
    LogicalBytes,
    /// Attempted calls to the top-level reader and writer interfaces.
    IoOperations,
    /// Combined requested scratch-buffer bytes, excluding backend allocations.
    ScratchBytes,
}
/// A caller's resource limit would be exceeded at an operation preflight boundary.
#[derive(Debug)]
pub struct OperationLimitExceeded {
    resource: OperationResource,
    limit: u64,
    requested: u128,
}
impl OperationLimitExceeded {
    /// Resource whose budget was exhausted.
    pub fn resource(&self) -> OperationResource {
        self.resource
    }
    /// Configured ceiling.
    pub fn limit(&self) -> u64 {
        self.limit
    }
    /// Total requested usage, including previous operations on the context.
    pub fn requested(&self) -> u128 {
        self.requested
    }
}
impl fmt::Display for OperationLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} requires {}, limit {}",
            self.resource, self.requested, self.limit
        )
    }
}
impl std::error::Error for OperationLimitExceeded {}

/// Cooperative cancellation at a documented operation boundary.
/// Completed writes remain; cancellation does not imply rollback or durability.
#[derive(Debug)]
pub struct OperationCancelled;
impl fmt::Display for OperationCancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("image operation cancelled")
    }
}
impl std::error::Error for OperationCancelled {}

/// Caller ceilings for operations using [`OperationContext`].
/// Byte and I/O ceilings default to `u64::MAX`; operation size still bounds work.
/// Scratch defaults to the existing comparison ceiling of 128 KiB combined.
/// Individual buffers remain at most 64 KiB; caller limits can only tighten it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationLimits {
    logical_bytes: u64,
    io_operations: u64,
    scratch_bytes: usize,
}
impl Default for OperationLimits {
    fn default() -> Self {
        Self {
            logical_bytes: u64::MAX,
            io_operations: u64::MAX,
            scratch_bytes: 131072,
        }
    }
}
impl OperationLimits {
    /// Set cumulative completed logical bytes. Zero permits empty work.
    #[must_use]
    pub fn logical_bytes(mut self, bytes: u64) -> Self {
        self.logical_bytes = bytes;
        self
    }
    /// Set cumulative top-level I/O calls, counting failed attempts.
    #[must_use]
    pub fn io_operations(mut self, calls: u64) -> Self {
        self.io_operations = calls;
        self
    }
    /// Tighten scratch-buffer size to a positive value of at most 128 KiB combined.
    pub fn scratch_bytes(mut self, bytes: usize) -> io::Result<Self> {
        if bytes == 0 || bytes > 131072 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scratch bytes must be 1..=131072",
            ));
        }
        self.scratch_bytes = bytes;
        Ok(self)
    }
}
/// Cumulative usage across operations on one context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OperationUsage {
    /// Logical bytes successfully processed in complete operation chunks.
    pub logical_bytes: u64,
    /// Calls attempted at top-level I/O interfaces, including failed calls.
    pub io_operations: u64,
    /// Largest requested scratch buffer; excludes allocator/backend overhead.
    pub peak_scratch_bytes: u64,
}
/// Phase of synchronous operation progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OperationPhase {
    /// Generic logical processing, copying, hashing or comparison.
    Processing,
    /// Scanning native allocation units for zero payloads.
    AllocationScan,
    /// Reading source bytes to materialize a new image.
    ImageExport,
    /// Comparing a staged image with its immutable logical source.
    OutputVerification,
    /// Checking the logical tail removed by a zero-tail shrink policy.
    TailValidation,
    /// Final cancellation boundary before publishing the staged image.
    Publication,
    /// Final cancellation boundary before publishing an image/manifest directory.
    GenerationPublication,
    /// Read-only container ownership validation; logical byte counters are zero.
    MetadataValidation,
    /// Reading every current logical byte for accessibility validation.
    PayloadValidation,
    /// Zeroing an existing image through bounded native backend calls.
    Zeroing,
    /// Final cancellation boundary before creating a native disk snapshot.
    NativeSnapshotCreation,
    /// Final cancellation boundary before deleting a native disk snapshot.
    NativeSnapshotDeletion,
    /// Final cancellation boundary before restoring a native disk snapshot.
    NativeSnapshotRevert,
    /// Final cancellation boundary before native discard or its explicit fallback.
    NativeDiscard,
    /// Final cancellation boundary before host storage preallocation.
    NativePreallocation,
    /// Final cancellation boundary before replacing an existing graph declaration.
    ManifestReplacement,
    /// Final cancellation boundary before a native capacity transaction.
    NativeResize,
    /// Final cancellation boundary before unlinking an owned leaf snapshot.
    SnapshotDeletion,
}
/// Progress at a cooperative boundary within the current operation phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationProgress {
    /// Work being performed; metadata validation does not measure logical bytes.
    pub phase: OperationPhase,
    /// Successfully completed logical bytes in this phase; zero for metadata work.
    pub completed_bytes: u64,
    /// Total logical bytes in this phase; zero when metadata has no byte measure.
    pub total_bytes: u64,
    /// Cumulative context usage, including this operation.
    pub usage: OperationUsage,
}
/// Non-overlapping measured components of hosted image operations.
/// Byte counts describe requested bytes, including failed attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(usize)]
pub enum OperationEvent {
    /// Reading the immutable logical source.
    SourceRead,
    /// Testing payload bytes for zero content.
    ZeroDetection,
    /// Writing payload to a private export file.
    PayloadWrite,
    /// Writing headers or allocation tables.
    MetadataWrite,
    /// Seeking within a private export file.
    Seek,
    /// Synchronizing an export file.
    Sync,
    /// Reopening staged output for validation.
    OutputReopen,
    /// Exact staged-output comparison (inclusive of its reads).
    ByteVerification,
    /// Validating container mappings.
    MappingValidation,
    /// Publishing verified output.
    Publication,
    /// Reading logical bytes during the allocation scan.
    AllocationRead,
}
/// Cumulative measurements for one component; failed attempts remain visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OperationMetrics {
    /// Number of attempted calls.
    pub calls: u64,
    /// Requested bytes; zero for operations without a byte measure.
    pub bytes: u64,
    /// Attempts returning an error.
    pub failures: u64,
    /// Wall time spent inside the measured calls.
    pub elapsed: std::time::Duration,
}
/// Opt-in shared profiling sink. Clones refer to the same counters.
///
/// Timings are component costs, not a complete operation total. Verification is
/// inclusive; do not add nested backend timings to it. Disabled contexts neither
/// read the clock nor acquire this sink's mutex. Enabled profiling adds clock and
/// synchronization overhead and must be measured separately from headline runs.
#[derive(Debug, Clone, Default)]
pub struct OperationProfiler {
    metrics: std::sync::Arc<std::sync::Mutex<[OperationMetrics; 11]>>,
}
impl OperationProfiler {
    /// Snapshot one component, including attempts that failed.
    pub fn metrics(&self, event: OperationEvent) -> OperationMetrics {
        self.metrics.lock().unwrap_or_else(|e| e.into_inner())[event as usize]
    }
    pub(crate) fn record(
        &self,
        event: OperationEvent,
        bytes: u64,
        started: std::time::Instant,
        success: bool,
    ) {
        let elapsed = started.elapsed();
        let mut metrics = self.metrics.lock().unwrap_or_else(|e| e.into_inner());
        let metric = &mut metrics[event as usize];
        metric.calls = metric.calls.saturating_add(1);
        metric.bytes = metric.bytes.saturating_add(bytes);
        metric.failures = metric.failures.saturating_add(u64::from(!success));
        metric.elapsed = metric.elapsed.saturating_add(elapsed);
    }
    pub(crate) fn measure<T, E>(
        &self,
        event: OperationEvent,
        bytes: u64,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let started = std::time::Instant::now();
        let result = action();
        self.record(event, bytes, started, result.is_ok());
        result
    }
}
/// Reusable synchronous context with a borrowed progress observer.
///
/// The observer returns `ControlFlow::Break(())` to cancel. Callbacks run outside
/// backend I/O and metadata transactions. Usage survives errors; failed I/O can
/// have partial effects beyond successfully completed bytes. Parser/backend work
/// and flush durability are not accounted by this context.
pub struct OperationContext<'a> {
    profiler: Option<OperationProfiler>,
    limits: OperationLimits,
    usage: OperationUsage,
    observer: Option<&'a mut dyn FnMut(OperationProgress) -> ControlFlow<()>>,
}
impl Default for OperationContext<'_> {
    fn default() -> Self {
        Self::new(OperationLimits::default())
    }
}
impl<'a> OperationContext<'a> {
    /// Start with validated caller ceilings and zero usage.
    pub fn new(limits: OperationLimits) -> Self {
        Self {
            profiler: None,
            limits,
            usage: OperationUsage::default(),
            observer: None,
        }
    }
    /// Borrow a synchronous progress/cancellation observer.
    #[must_use]
    pub fn with_observer(
        mut self,
        observer: &'a mut dyn FnMut(OperationProgress) -> ControlFlow<()>,
    ) -> Self {
        self.observer = Some(observer);
        self
    }
    /// Enable component profiling into a caller-owned sink.
    #[must_use]
    pub fn with_profiler(mut self, profiler: OperationProfiler) -> Self {
        self.profiler = Some(profiler);
        self
    }
    /// Return the enabled profiling sink, if any.
    pub fn profiler(&self) -> Option<&OperationProfiler> {
        self.profiler.as_ref()
    }
    /// Snapshot cumulative usage without resetting accounting.
    pub fn usage(&self) -> OperationUsage {
        self.usage
    }
    pub(crate) fn preflight(
        &self,
        bytes: u64,
        buffers: usize,
        calls_per_chunk: u64,
    ) -> io::Result<usize> {
        if bytes == 0 {
            return Ok(0);
        }
        let chunk = (self.limits.scratch_bytes / buffers).min(65536);
        if chunk == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                OperationLimitExceeded {
                    resource: OperationResource::ScratchBytes,
                    limit: self.limits.scratch_bytes as u64,
                    requested: buffers as u128,
                },
            ));
        }
        self.preflight_chunks(bytes, chunk as u64, calls_per_chunk)?;
        Ok(bytes.min(chunk as u64) as usize)
    }
    pub(crate) fn preflight_chunks(
        &self,
        bytes: u64,
        chunk: u64,
        calls_per_chunk: u64,
    ) -> io::Result<()> {
        let calls = u128::from(bytes.div_ceil(chunk)) * u128::from(calls_per_chunk);
        for (resource, limit, requested) in [
            (
                OperationResource::LogicalBytes,
                self.limits.logical_bytes,
                u128::from(self.usage.logical_bytes) + u128::from(bytes),
            ),
            (
                OperationResource::IoOperations,
                self.limits.io_operations,
                u128::from(self.usage.io_operations) + calls,
            ),
        ] {
            if requested > u128::from(limit) {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    OperationLimitExceeded {
                        resource,
                        limit,
                        requested,
                    },
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn preflight_io(&self, calls: u128) -> io::Result<()> {
        let requested = u128::from(self.usage.io_operations) + calls;
        if requested > u128::from(self.limits.io_operations) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                OperationLimitExceeded {
                    resource: OperationResource::IoOperations,
                    limit: self.limits.io_operations,
                    requested,
                },
            ));
        }
        Ok(())
    }
    pub(crate) fn observe(&mut self, completed: u64, total: u64) -> io::Result<()> {
        self.observe_phase(OperationPhase::Processing, completed, total)
    }
    pub(crate) fn observe_phase(
        &mut self,
        phase: OperationPhase,
        completed: u64,
        total: u64,
    ) -> io::Result<()> {
        if let Some(observer) = &mut self.observer
            && observer(OperationProgress {
                phase,
                completed_bytes: completed,
                total_bytes: total,
                usage: self.usage,
            })
            .is_break()
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                OperationCancelled,
            ));
        }
        Ok(())
    }
    pub(crate) fn require_scratch(&self, bytes: usize) -> io::Result<()> {
        if bytes > self.limits.scratch_bytes {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                OperationLimitExceeded {
                    resource: OperationResource::ScratchBytes,
                    limit: self.limits.scratch_bytes as u64,
                    requested: bytes as u128,
                },
            ));
        }
        Ok(())
    }
    pub(crate) fn scratch(&mut self, bytes: usize) {
        self.usage.peak_scratch_bytes = self.usage.peak_scratch_bytes.max(bytes as u64);
    }
    pub(crate) fn attempted_io(&mut self) {
        self.usage.io_operations += 1;
    }
    pub(crate) fn completed(&mut self, bytes: u64) {
        self.usage.logical_bytes += bytes;
    }
}

pub(crate) fn scratch_buffer(bytes: usize) -> io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
    buffer.resize(bytes, 0);
    Ok(buffer)
}
