//! Caller-tightened parser budgets and typed read provenance.
use std::{
    fmt, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Parser ceilings. Callers may tighten defaults, never silently remove them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserLimits {
    /// Cumulative metadata materialized by one parser operation.
    pub metadata_bytes: u64,
    /// Simultaneously retained parser cache bytes.
    pub cache_bytes: u64,
    /// Traversal depth; format-specific smaller ceilings still apply.
    pub recursion_depth: u64,
    /// Total positional reads and format work items.
    pub work_items: u64,
    /// Total output produced by compression decoders, including repeated reads.
    pub decompressed_bytes: u64,
    /// Maximum scratch/output buffer for one compression unit.
    pub decompression_buffer_bytes: u64,
    /// Maximum materialized metadata attribute.
    pub attribute_bytes: u64,
    /// Maximum extension records for one NTFS attribute list.
    pub attribute_list_records: u64,
}
impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            metadata_bytes: 2 << 30,
            cache_bytes: 512 << 20,
            recursion_depth: 512,
            work_items: 16 << 20,
            decompressed_bytes: 64 << 30,
            decompression_buffer_bytes: 2 << 20,
            attribute_bytes: 64 << 20,
            attribute_list_records: 65536,
        }
    }
}
impl ParserLimits {
    /// Refuse zero or a value above the corresponding hard default ceiling.
    pub fn validate(&self) -> io::Result<()> {
        let hard = Self::default();
        for (name, value, ceiling) in [
            ("metadata", self.metadata_bytes, hard.metadata_bytes),
            ("cache", self.cache_bytes, hard.cache_bytes),
            ("recursion", self.recursion_depth, hard.recursion_depth),
            ("work", self.work_items, hard.work_items),
            (
                "decompressed output",
                self.decompressed_bytes,
                hard.decompressed_bytes,
            ),
            (
                "decompression buffer",
                self.decompression_buffer_bytes,
                hard.decompression_buffer_bytes,
            ),
            ("attribute", self.attribute_bytes, hard.attribute_bytes),
            (
                "attribute-list records",
                self.attribute_list_records,
                hard.attribute_list_records,
            ),
        ] {
            if value == 0 || value > ceiling {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} parser limit must be positive and at most {ceiling}"),
                ));
            }
        }
        Ok(())
    }
}

/// Typed provenance retained without converting original UTF-16 names to UTF-8.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadContext {
    /// Physical container path, when available.
    pub container: Option<PathBuf>,
    /// Selected partition slot.
    pub partition: Option<u32>,
    /// NTFS file reference, including sequence number when known.
    pub record: Option<u64>,
    /// NTFS attribute type code.
    pub attribute: Option<u32>,
    /// Original stream name; empty means unnamed data.
    pub stream: Option<Vec<u16>>,
    /// Original volume-relative UTF-16 components.
    pub path: Option<Vec<Vec<u16>>>,
    /// Offset in the reader named by this context.
    pub offset: Option<u64>,
}
impl ReadContext {
    /// Preserve the original error kind and source inside typed provenance.
    pub fn error(self, operation: &'static str, source: io::Error) -> io::Error {
        io::Error::new(
            source.kind(),
            ReadError {
                context: self,
                operation,
                source,
            },
        )
    }
}
/// A contextual library error, usable through `io::Error::get_ref()` and sources.
#[derive(Debug)]
pub struct ReadError {
    /// Structured physical, partition and filesystem provenance.
    pub context: ReadContext,
    /// Operation that established this context.
    pub operation: &'static str,
    source: io::Error,
}
impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.operation)?;
        if let Some(p) = &self.context.container {
            write!(f, " container={}", p.display())?;
        }
        if let Some(p) = self.context.partition {
            write!(f, " partition={p}")?;
        }
        if let Some(r) = self.context.record {
            write!(f, " record={r:#x}")?;
        }
        if let Some(a) = self.context.attribute {
            write!(f, " attribute={a:#x}")?;
        }
        if let Some(p) = &self.context.path {
            for c in p {
                write!(f, "/{}", String::from_utf16_lossy(c))?;
            }
        }
        if let Some(s) = &self.context.stream {
            write!(f, " stream={:?}", String::from_utf16_lossy(s))?;
        }
        if let Some(o) = self.context.offset {
            write!(f, " offset={o}")?;
        }
        write!(f, ": {}", self.source)
    }
}
impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
struct Accounting {
    limits: ParserLimits,
    metadata: AtomicU64,
    cache: AtomicU64,
    work: AtomicU64,
    decoded: AtomicU64,
}
/// Current shared parser accounting, read atomically without resetting limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadBudgetUsage {
    /// Cumulative materialized metadata bytes.
    pub metadata_bytes: u64,
    /// Live retained cache bytes.
    pub cache_bytes: u64,
    /// Cumulative physical-read and format-iteration work.
    pub work_items: u64,
    /// Cumulative decoded output bytes.
    pub decompressed_bytes: u64,
}
/// Shared accounting retained by deferred readers. Counters never wrap.
#[derive(Debug, Clone)]
pub struct ReadBudget(Arc<Accounting>);
fn charge(counter: &AtomicU64, amount: u64, limit: u64, name: &str) -> io::Result<()> {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
            old.checked_add(amount).filter(|&next| next <= limit)
        })
        .map(|_| ())
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{name} exceeds configured parser limit {limit}"),
            )
        })
}
impl ReadBudget {
    /// Start a bounded parser operation with validated caller limits.
    pub fn new(limits: ParserLimits) -> io::Result<Self> {
        limits.validate()?;
        Ok(Self(Arc::new(Accounting {
            limits,
            metadata: AtomicU64::new(0),
            cache: AtomicU64::new(0),
            work: AtomicU64::new(0),
            decoded: AtomicU64::new(0),
        })))
    }
    /// Snapshot counters; concurrent readers may advance individual fields.
    pub fn usage(&self) -> ReadBudgetUsage {
        ReadBudgetUsage {
            metadata_bytes: self.0.metadata.load(Ordering::Acquire),
            cache_bytes: self.0.cache.load(Ordering::Acquire),
            work_items: self.0.work.load(Ordering::Acquire),
            decompressed_bytes: self.0.decoded.load(Ordering::Acquire),
        }
    }
    /// Effective immutable limits.
    pub fn limits(&self) -> ParserLimits {
        self.0.limits
    }
    /// Charge cumulative metadata before materializing it.
    pub fn metadata(&self, bytes: u64) -> io::Result<()> {
        charge(
            &self.0.metadata,
            bytes,
            self.0.limits.metadata_bytes,
            "metadata",
        )
    }
    /// Charge positional reads or format iteration work before doing it.
    pub fn work(&self, items: u64) -> io::Result<()> {
        charge(&self.0.work, items, self.0.limits.work_items, "work")
    }
    /// Charge output before allocating or decoding a compression unit.
    pub fn decode(&self, bytes: u64) -> io::Result<()> {
        if bytes > self.0.limits.decompression_buffer_bytes {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "compression unit exceeds configured decompression-buffer limit",
            ));
        }
        charge(
            &self.0.decoded,
            bytes,
            self.0.limits.decompressed_bytes,
            "decoded output",
        )
    }
    /// Reserve live cache bytes; dropping the returned reservation releases them.
    pub fn cache(&self, bytes: u64) -> io::Result<CacheReservation> {
        charge(&self.0.cache, bytes, self.0.limits.cache_bytes, "cache")?;
        Ok(CacheReservation {
            budget: self.clone(),
            bytes,
        })
    }
    /// Wrap positional reads with shared work accounting and original provenance.
    pub fn reader(&self, source: Arc<dyn crate::ReadAt>) -> Arc<dyn crate::ReadAt> {
        Arc::new(BudgetReader {
            source,
            budget: self.clone(),
        })
    }
}
/// Lifetime of retained cache storage. Reservations cannot be copied.
#[derive(Debug)]
pub struct CacheReservation {
    budget: ReadBudget,
    bytes: u64,
}
impl Drop for CacheReservation {
    fn drop(&mut self) {
        self.budget.0.cache.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
struct BudgetReader {
    source: Arc<dyn crate::ReadAt>,
    budget: ReadBudget,
}
impl crate::ReadAt for BudgetReader {
    fn len(&self) -> u64 {
        self.source.len()
    }
    fn sparse_holes(&self) -> std::io::Result<Vec<(u64, u64)>> {
        self.source.sparse_holes()
    }
    fn context(&self) -> ReadContext {
        self.source.context()
    }
    fn budget(&self) -> Option<ReadBudget> {
        Some(self.budget.clone())
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.budget
            .work(1)
            .map_err(|e| self.context().error("read budget", e))?;
        self.source.read_exact_at(offset, bytes)
    }
}
struct ContextReader {
    source: Arc<dyn crate::ReadAt>,
    context: ReadContext,
}
/// Annotate a reader without reopening it or changing its shared budget.
pub fn contextual_reader(
    source: Arc<dyn crate::ReadAt>,
    context: ReadContext,
) -> Arc<dyn crate::ReadAt> {
    Arc::new(ContextReader { source, context })
}
impl crate::ReadAt for ContextReader {
    fn len(&self) -> u64 {
        self.source.len()
    }
    fn sparse_holes(&self) -> std::io::Result<Vec<(u64, u64)>> {
        self.source.sparse_holes()
    }
    fn context(&self) -> ReadContext {
        self.context.clone()
    }
    fn budget(&self) -> Option<ReadBudget> {
        self.source.budget()
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.source.read_exact_at(offset, bytes).map_err(|e| {
            let mut c = self.context();
            c.offset = Some(offset);
            c.error("read stream", e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_reservations_release_and_cumulative_limits_do_not_reset() {
        let budget = ReadBudget::new(ParserLimits {
            cache_bytes: 8,
            metadata_bytes: 8,
            ..Default::default()
        })
        .unwrap();
        let lease = budget.cache(8).unwrap();
        assert_eq!(
            budget.cache(1).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        drop(lease);
        drop(budget.cache(8).unwrap());
        budget.metadata(8).unwrap();
        assert_eq!(
            budget.metadata(1).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
    #[test]
    fn typed_provenance_keeps_original_names_kind_and_cause() {
        let context = ReadContext {
            partition: Some(3),
            record: Some(0x1000000000017),
            attribute: Some(0x80),
            stream: Some(vec![0xd800]),
            ..Default::default()
        };
        let error = context.clone().error(
            "read stream",
            io::Error::new(io::ErrorKind::Interrupted, "cancelled"),
        );
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        let typed = error
            .get_ref()
            .unwrap()
            .downcast_ref::<ReadError>()
            .unwrap();
        assert_eq!(typed.context, context);
        assert_eq!(
            std::error::Error::source(typed).unwrap().to_string(),
            "cancelled"
        );
    }
}
