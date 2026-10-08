//! Logical-content-preserving sparse rewrites.
use crate::io;
use crate::{ImageFormat, ReadAt};
use std::{ops::ControlFlow, path::Path};

/// Compact into a new independent image, omitting zero-filled payload units.
///
/// Preserves capacity and logical bytes, flattens inheritance, and never edits
/// guest filesystems. Nonzero bytes are retained regardless of guest allocation.
/// This scans the immutable logical source; it does not interpret free space.
/// QCOW2 metadata tables remain allocated. Raw hole storage depends on the host
/// filesystem. Output size can grow for another profile or allocation unit.
/// The output is verified and published without overwrite, using host hard links.
pub fn compact_image(
    source: &dyn ReadAt,
    output: impl AsRef<Path>,
    format: ImageFormat,
) -> io::Result<()> {
    compact_image_with_context(
        source,
        output,
        format,
        &mut crate::OperationContext::default(),
    )
}

/// Compact and verify a staged sparse image with common budgets and cancellation.
///
/// Uses the accounting and publication contract of [`crate::convert_image_with_context`].
/// Zero-allocation scans are separate phases; no guest filesystem is interpreted.
/// Cancellation or quotas before publication leave the output absent and staging
/// cleanup is best effort. Backend metadata and flush work are not interruptible.
pub fn compact_image_with_context(
    source: &dyn ReadAt,
    output: impl AsRef<Path>,
    format: ImageFormat,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<()> {
    crate::image::materialize_with_context(source, output.as_ref(), format, true, context)
}

/// Compact a new output with cancellation checked between logical reads and before publication.
///
/// Cancellation removes unpublished staging files on a best-effort basis and
/// returns `Interrupted`. It cannot interrupt an individual host I/O call or
/// revoke a completed publication. The source remains immutable.
pub fn compact_image_with_cancel(
    source: &dyn ReadAt,
    output: impl AsRef<Path>,
    format: ImageFormat,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> io::Result<()> {
    let proxy = Cancellable { source, cancelled };
    proxy.check()?;
    // Retain the legacy checks before source reads, with only publication
    // cancellation delegated to the operation observer.
    let mut observer = |progress: crate::OperationProgress| {
        if progress.phase == crate::OperationPhase::Publication && cancelled() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = crate::OperationContext::default().with_observer(&mut observer);
    compact_image_with_context(&proxy, output, format, &mut context)
}

struct Cancellable<'a> {
    source: &'a dyn ReadAt,
    cancelled: &'a (dyn Fn() -> bool + Send + Sync),
}
impl Cancellable<'_> {
    fn check(&self) -> io::Result<()> {
        if (self.cancelled)() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "compaction cancelled",
            ));
        }
        Ok(())
    }
}
impl ReadAt for Cancellable<'_> {
    fn source_identity(&self) -> Option<crate::SourceIdentity> {
        self.source.source_identity()
    }
    fn ancestor_identities(&self) -> Vec<crate::SourceIdentity> {
        self.source.ancestor_identities()
    }
    fn host_context(&self) -> Option<&dyn core::any::Any> {
        self.source.host_context()
    }
    fn len(&self) -> u64 {
        self.source.len()
    }
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.check()?;
        self.source.read_exact_at(offset, destination)
    }
    fn context(&self) -> crate::ReadContext {
        self.source.context()
    }
    fn budget(&self) -> Option<crate::ReadBudget> {
        self.source.budget()
    }
}
