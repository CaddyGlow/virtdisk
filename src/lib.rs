//! Bounded virtual disk readers and explicit offline writable profiles.
//!
//! Callers must keep the underlying image immutable for the entire capture and
//! any deferred reads. Read-only handles do not prevent another process writing
//! the source. QCOW2 active mappings are validated; partition tables are
//! provided separately by `partmgr` and filesystem interpretation by
//! `disk-capture`.
#![deny(missing_docs)]

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

mod policy;
#[cfg(test)]
mod test_sync;
mod transaction;
#[cfg(target_os = "linux")]
mod transaction_set;
pub use policy::{
    CacheReservation, ParserLimitExceeded, ParserLimits, ParserResource, ReadBudget,
    ReadBudgetUsage, ReadContext, ReadError, contextual_reader,
};

mod qcow2;
pub use qcow2::{Qcow2, Qcow2Snapshot, Qcow2SnapshotView, Qcow2Validation};

mod raw_write;
pub use raw_write::RawWriter;
mod vdi;
pub use vdi::Vdi;
mod vmdk;
pub use vmdk::Vmdk;
mod image;
mod image_writer;
mod operation;
pub use image::{
    Image, ImageFormat, ImageInfo, ShrinkPolicy, compare_images, compare_images_with_context,
    convert_image, convert_image_with_context, copy_image, copy_image_with_cancel,
    copy_image_with_context, detect_format, hash_image, hash_image_with_context, resize_image,
    resize_image_with_context,
};
pub use image_writer::ImageWriter;
pub use operation::OperationError;
mod operation_context;
pub use operation_context::{
    OperationCancelled, OperationContext, OperationLimitExceeded, OperationLimits, OperationPhase,
    OperationProgress, OperationResource, OperationUsage,
};
mod writer_open;
pub use writer_open::{RecoveryPolicy, RecoveryRequired, WriterOpenOptions};
mod reader_open;
pub use reader_open::{ReadRecoveryPolicy, ReaderOpenOptions};
mod export_source;
mod qcow2_write;
pub use qcow2_write::{create_qcow2, create_sparse_qcow2};
mod qcow2_writer;
pub use qcow2_writer::Qcow2Writer;
mod qcow2_overlay;
pub use qcow2_overlay::{create_qcow2_overlay, create_qcow2_overlay_with_chain};
mod graph;
mod graph_manifest;
pub use graph::{ImageGraph, ImageSpec};
pub use graph_manifest::GraphManifest;
mod compact;
pub use compact::{compact_image, compact_image_with_cancel, compact_image_with_context};
mod check;
pub use check::{
    CheckOptions, CheckReport, CheckScope, check_image, check_image_with_cancel,
    check_image_with_context, check_image_with_limits, check_image_with_limits_and_context,
    check_payload_with_cancel, check_payload_with_context,
};
mod vdi_write;
pub use vdi_write::{create_vdi, create_vdi_overlay};
mod vdi_writer;
pub use vdi_writer::VdiWriter;
mod vhdx;
pub use vhdx::Vhdx;
mod vhdx_write;
pub use vhdx_write::{create_vhdx, create_vhdx_overlay};
mod vhdx_writer;
pub use vhdx_writer::VhdxWriter;
mod vhdx_recover;
pub use vhdx_recover::{recover_vhdx, recover_vhdx_chain};
mod vmdk_write;
pub use vmdk_write::create_vmdk;
mod vmdk_writer;
pub use vmdk_writer::VmdkWriter;
mod write;
pub use write::{DiscardPolicy, DiscardResult, WriteAt, zero_image_with_context};
mod info;
pub use info::{
    Capability, DiskGeometry, ImageCapabilities, ImageInspection, ImageOperation, ImageProfile,
    InspectImage, UnsupportedReason, ValidationLevel,
};

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

    /// Fill the destination at the given logical byte offset.
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()>;
}

fn check_range(offset: u64, count: u64, length: u64) -> io::Result<()> {
    if offset.checked_add(count).is_none_or(|end| end > length) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "disk read range overflows or exceeds logical length",
        ));
    }
    Ok(())
}

/// A raw image opened read-only and retained through all deferred reads.
///
/// The private file cursor is serialized for portable exact reads. This does
/// not lock out external writers or provide a snapshot. Use an immutable source.
pub struct RawDisk {
    file: Mutex<File>,
    length: u64,
    context: ReadContext,
}

impl RawDisk {
    pub(crate) fn identity(&self) -> io::Result<same_file::Handle> {
        let file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("disk reader mutex poisoned"))?;
        same_file::Handle::from_file(file.try_clone()?)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn open_shared_locked(path: &Path) -> io::Result<Self> {
        let file: File = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )?
        .into();
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "dependency must be a regular file",
            ));
        }
        file.try_lock_shared().map_err(io::Error::from)?;
        Ok(Self {
            length: metadata.len(),
            file: Mutex::new(file),
            context: ReadContext {
                container: Some(path.to_path_buf()),
                ..Default::default()
            },
        })
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn pin_metadata(&self) -> io::Result<std::fs::Metadata> {
        self.file
            .lock()
            .map_err(|_| io::Error::other("disk reader mutex poisoned"))?
            .metadata()
    }
    /// Open a regular raw image. Device files and directories are rejected.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let context = ReadContext {
            container: Some(path.to_path_buf()),
            ..Default::default()
        };
        let file = File::open(path).map_err(|e| context.clone().error("open raw image", e))?;
        let metadata = file
            .metadata()
            .map_err(|e| context.clone().error("inspect raw image", e))?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "raw image must be a regular file",
            ));
        }
        Ok(Self {
            file: Mutex::new(file),
            length: metadata.len(),
            context: ReadContext {
                container: Some(path.canonicalize()?),
                ..context
            },
        })
    }
}

impl ReadAt for RawDisk {
    fn context(&self) -> ReadContext {
        self.context.clone()
    }
    fn len(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        let result = (|| {
            use std::io::{Read, Seek, SeekFrom};
            check_range(offset, destination.len() as u64, self.length)?;
            let mut file = self
                .file
                .lock()
                .map_err(|_| io::Error::other("disk reader mutex poisoned"))?;
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(destination)
        })();
        result.map_err(|e| {
            let mut context = self.context();
            context.offset = Some(offset);
            context.error("read raw image", e)
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture() -> (tempfile::NamedTempFile, Arc<dyn ReadAt>) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&(0..=255u8).collect::<Vec<_>>()).unwrap();
        let disk = Arc::new(RawDisk::open(file.path()).unwrap());
        (file, disk)
    }

    #[test]
    fn nested_views_translate_offsets_and_reject_boundary_crossing() {
        let (_file, disk) = fixture();
        let outer = Arc::new(DiskView::new(disk, 20, 100).unwrap());
        let inner = DiskView::new(outer, 10, 4).unwrap();
        let mut data = [0; 4];
        inner.read_exact_at(0, &mut data).unwrap();
        assert_eq!(data, [30, 31, 32, 33]);
        assert!(inner.read_exact_at(1, &mut data).is_err());
        assert!(inner.read_exact_at(4, &mut []).is_ok());
        assert!(inner.read_exact_at(5, &mut []).is_err());
        assert!(inner.read_exact_at(u64::MAX, &mut data).is_err());
    }

    #[test]
    fn view_construction_rejects_overflow_and_out_of_bounds() {
        let (_file, disk) = fixture();
        assert!(DiskView::new(disk.clone(), u64::MAX, 2).is_err());
        assert!(DiskView::new(disk.clone(), 255, 2).is_err());
        assert!(DiskView::new(disk, 256, 0).is_ok());
    }

    #[test]
    fn simultaneous_reads_do_not_share_cursor_positions() {
        let (_file, disk) = fixture();
        std::thread::scope(|scope| {
            for offset in 0..32u64 {
                let disk = &disk;
                scope.spawn(move || {
                    for _ in 0..100 {
                        let mut data = [0; 8];
                        disk.read_exact_at(offset, &mut data).unwrap();
                        assert_eq!(data, std::array::from_fn(|i| offset as u8 + i as u8));
                    }
                });
            }
        });
    }

    #[test]
    fn truncation_is_an_error_instead_of_zero_filled_data() {
        let (file, disk) = fixture();
        file.as_file().set_len(16).unwrap();
        assert_eq!(
            disk.read_exact_at(20, &mut [0; 4]).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
