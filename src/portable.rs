//! Storage contracts independent of host files and paths.
use crate::{ReadBudget, ReadContext, io};
use alloc::{sync::Arc, vec::Vec};
/// Logical allocation classification; it does not identify guest free space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentKind {
    /// Container payload allocation, regardless of its byte content.
    Allocated,
    /// Reads return zero without consulting a parent.
    Zero,
    /// Logical bytes are resolved through an immutable backing image.
    Inherited,
    /// Allocation information is unavailable for this reader.
    Unknown,
}

/// One ordered, nonempty logical allocation extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskExtent {
    /// Logical starting byte offset.
    pub offset: u64,
    /// Extent length in bytes, wholly within the reader.
    pub length: u64,
    /// Logical allocation semantics.
    pub kind: ExtentKind,
}

/// An exact positional reader with a fixed logical length.
///
/// Implementations must reject overflowing/out-of-bounds ranges, including an
/// empty read beyond the end. Reads may partially modify the destination on I/O
/// failure; callers must discard it on error. Implementations must be safe to
/// share across threads without a shared seek cursor affecting results.
pub trait ReadAt: Send + Sync {
    /// Logical length in bytes, fixed for this reader's lifetime.
    fn len(&self) -> u64;

    /// Visit ordered extents covering the disk using bounded scratch memory.
    ///
    /// Adjacent extents may share a kind. Returning an error from the visitor
    /// stops traversal immediately, permitting cancellation. The default
    /// conservatively reports unknown allocation without reading payload data.
    fn visit_extents(
        &self,
        visitor: &mut dyn FnMut(DiskExtent) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.len() != 0 {
            visitor(DiskExtent {
                offset: 0,
                length: self.len(),
                kind: ExtentKind::Unknown,
            })?;
        }
        Ok(())
    }

    /// Logical sparse hole ranges as half-open byte offsets. Empty means no known holes.
    fn sparse_holes(&self) -> io::Result<Vec<(u64, u64)>> {
        Ok(Vec::new())
    }

    /// Whether the logical image is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Original physical/partition/filesystem provenance, when available.
    fn context(&self) -> ReadContext {
        ReadContext::default()
    }

    /// Shared parser accounting retained through deferred reads, when configured.
    fn budget(&self) -> Option<ReadBudget> {
        None
    }

    /// Optional host adapter state, opaque to portable parsers.
    fn host_context(&self) -> Option<&dyn core::any::Any> {
        None
    }

    /// Trusted provider identity of the retained container, when available.
    fn source_identity(&self) -> Option<SourceIdentity> {
        None
    }

    /// Container identities on this reader's retained parent path.
    fn ancestor_identities(&self) -> Vec<SourceIdentity> {
        Vec::new()
    }

    /// Fill the destination at the given logical byte offset.
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()>;
}

pub(crate) fn check_range(offset: u64, count: u64, length: u64) -> io::Result<()> {
    if offset.checked_add(count).is_none_or(|end| end > length) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "disk read range overflows or exceeds logical length",
        ));
    }
    Ok(())
}

/// A bounded logical subrange retaining its parent reader.
///
/// Suitable for partition views once a partition parser has validated the
/// layout. Construction alone does not establish partition validity.
pub struct DiskView {
    source: Arc<dyn ReadAt>,
    start: u64,
    length: u64,
    context: ReadContext,
}

impl DiskView {
    /// Create a view wholly contained in the parent reader.
    pub fn new(source: Arc<dyn ReadAt>, start: u64, length: u64) -> io::Result<Self> {
        check_range(start, length, source.len())?;
        let context = source.context();
        Ok(Self {
            source,
            start,
            length,
            context,
        })
    }
    /// Attach the validated selected partition slot to this bounded view.
    pub fn with_partition(mut self, index: u32) -> Self {
        self.context.partition = Some(index);
        self
    }
}

impl ReadAt for DiskView {
    fn host_context(&self) -> Option<&dyn core::any::Any> {
        self.source.host_context()
    }
    fn source_identity(&self) -> Option<SourceIdentity> {
        self.source
            .source_identity()?
            .region(self.start, self.length)
            .ok()
    }
    fn ancestor_identities(&self) -> Vec<SourceIdentity> {
        self.source.ancestor_identities()
    }

    fn visit_extents(
        &self,
        visitor: &mut dyn FnMut(DiskExtent) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.length == 0 {
            return Ok(());
        }
        let end = self.start + self.length;
        self.source.visit_extents(&mut |extent| {
            check_range(extent.offset, extent.length, self.source.len())?;
            let start = extent.offset.max(self.start);
            let stop = (extent.offset + extent.length).min(end);
            if start < stop {
                visitor(DiskExtent {
                    offset: start - self.start,
                    length: stop - start,
                    kind: extent.kind,
                })?;
            }
            Ok(())
        })
    }
    fn context(&self) -> ReadContext {
        self.context.clone()
    }
    fn budget(&self) -> Option<ReadBudget> {
        self.source.budget()
    }
    fn len(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        let result = (|| {
            check_range(offset, destination.len() as u64, self.length)?;
            let absolute = self.start.checked_add(offset).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "disk view offset overflow")
            })?;
            self.source.read_exact_at(absolute, destination)
        })();
        result.map_err(|e| {
            let mut context = self.context();
            context.offset = Some(offset);
            context.error("read partition view", e)
        })
    }
}

/// Trusted provider token identifying retained storage and a container region.
///
/// Providers must choose a globally distinct namespace and keep storage tokens
/// stable for aliases throughout the lifetime of every retained graph. Tokens
/// are authority supplied by a trusted provider, not hashes or diagnostic labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceIdentity {
    namespace: u128,
    storage: u128,
    start: u64,
    length: u64,
}
impl SourceIdentity {
    /// Issue an identity for a complete retained storage object.
    pub const fn new(namespace: u128, storage: u128, length: u64) -> Self {
        Self {
            namespace,
            storage,
            start: 0,
            length,
        }
    }
    /// Provider namespace, which must not collide with independent providers.
    pub const fn namespace(self) -> u128 {
        self.namespace
    }
    /// Provider-issued underlying storage token.
    pub const fn storage(self) -> u128 {
        self.storage
    }
    /// Starting byte of the retained container region in storage.
    pub const fn start(self) -> u64 {
        self.start
    }
    /// Length of the retained container region.
    pub const fn length(self) -> u64 {
        self.length
    }
    /// Whether two containers retain regions of the same underlying storage.
    pub const fn same_storage(self, other: Self) -> bool {
        self.namespace == other.namespace && self.storage == other.storage
    }
    /// Derive a checked embedded-container identity from a retained region.
    pub fn region(self, offset: u64, length: u64) -> io::Result<Self> {
        check_range(offset, length, self.length)?;
        let start = self.start.checked_add(offset).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "identity region overflows")
        })?;
        Ok(Self {
            start,
            length,
            ..self
        })
    }
}

/// Validate a supplied parent path, refusing absent identity and repeated tokens.
///
/// Native format linkage is validated by the format parser before it exposes a
/// parented reader. Shared ancestors on separate paths remain permitted.
pub fn validate_parent_identity(
    source: &dyn ReadAt,
    parent: &dyn ReadAt,
    budget: &ReadBudget,
    profile_depth: u64,
) -> io::Result<()> {
    let child = source.source_identity().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::MissingIdentity,
            "child storage identity unavailable",
        )
    })?;
    let parent_token = parent.source_identity().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::MissingIdentity,
            "parent storage identity unavailable",
        )
    })?;
    let ancestors = parent.ancestor_identities();
    budget.recursion(ancestors.len() as u128 + 2, profile_depth)?;
    if child == parent_token || ancestors.contains(&child) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "repeated ancestor storage identity",
        ));
    }
    for (index, token) in ancestors.iter().enumerate() {
        if *token == parent_token || ancestors[..index].contains(token) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "repeated ancestor storage identity",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    struct Memory {
        bytes: Vec<u8>,
        identity: Option<SourceIdentity>,
        ancestors: Vec<SourceIdentity>,
    }
    impl ReadAt for Memory {
        fn len(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn source_identity(&self) -> Option<SourceIdentity> {
            self.identity
        }
        fn ancestor_identities(&self) -> Vec<SourceIdentity> {
            self.ancestors.clone()
        }
        fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
            check_range(offset, output.len() as u64, self.len())?;
            output.copy_from_slice(&self.bytes[offset as usize..offset as usize + output.len()]);
            Ok(())
        }
    }
    fn memory(token: Option<SourceIdentity>) -> Memory {
        Memory {
            bytes: vec![0, 1, 2, 3],
            identity: token,
            ancestors: Vec::new(),
        }
    }
    #[test]
    fn views_preserve_empty_range_and_overflow_contracts() {
        let source = Arc::new(memory(Some(SourceIdentity::new(7, 1, 4))));
        let view = DiskView::new(source.clone(), 1, 2).unwrap();
        let mut bytes = [0; 2];
        view.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [1, 2]);
        assert!(view.read_exact_at(2, &mut []).is_ok());
        assert_eq!(
            view.read_exact_at(3, &mut []).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(DiskView::new(source, u64::MAX, 2).is_err());
        assert_eq!(view.source_identity().unwrap().start(), 1);
    }
    #[test]
    fn graph_identity_detects_aliases_and_preserves_namespaces() {
        let token = SourceIdentity::new(7, 1, 4);
        let child = memory(Some(token));
        let alias = memory(Some(token));
        let distinct = memory(Some(SourceIdentity::new(7, 2, 4)));
        let independent = memory(Some(SourceIdentity::new(8, 1, 4)));
        let budget = ReadBudget::new(crate::ParserLimits::default()).unwrap();
        assert!(validate_parent_identity(&child, &alias, &budget, 32).is_err());
        validate_parent_identity(&child, &distinct, &budget, 32).unwrap();
        validate_parent_identity(&child, &independent, &budget, 32).unwrap();
        assert_eq!(
            validate_parent_identity(&child, &memory(None), &budget, 32)
                .unwrap_err()
                .kind(),
            io::ErrorKind::MissingIdentity
        );
        let mut repeated = distinct;
        repeated.ancestors.push(token);
        assert!(validate_parent_identity(&child, &repeated, &budget, 32).is_err());
    }
}
