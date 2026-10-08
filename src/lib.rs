//! Bounded virtual disk readers and explicit offline writable profiles.
//!
//! Callers must keep the underlying image immutable for the entire capture and
//! any deferred reads. Read-only handles do not prevent another process writing
//! the source. QCOW2 active mappings are validated; partition tables are
//! provided separately by `partmgr` and filesystem interpretation by
//! `disk-capture`.
#![deny(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]
extern crate alloc;
pub mod io;
pub mod portable;
use portable::check_range;
pub use portable::{DiskExtent, DiskView, ExtentKind, ReadAt, SourceIdentity};

#[cfg(feature = "std")]
use std::fs::File;

#[cfg(feature = "std")]
use std::path::Path;
#[cfg(feature = "std")]
use std::sync::Mutex;

#[cfg(feature = "std")]
mod host_source;
#[cfg(feature = "std")]
pub use host_source::HostSourceContext;
mod format;
pub use format::{ImageFormat, detect_format};
mod crc32c;
#[cfg(feature = "std")]
mod native_id;
mod policy;
#[cfg(feature = "std")]
mod sidecar;
#[cfg(feature = "std")]
mod source;
#[cfg(test)]
#[cfg(feature = "std")]
mod test_sync;
#[cfg(feature = "std")]
mod transaction;
#[cfg(target_os = "linux")]
#[cfg(feature = "std")]
mod transaction_set;
mod vmdk_descriptor;
pub use policy::{
    CacheReservation, ParserLimitExceeded, ParserLimits, ParserResource, ReadBudget,
    ReadBudgetUsage, ReadContext, ReadError, contextual_reader,
};

mod qcow2;
mod zstd_decoder;
pub use qcow2::{Qcow2, Qcow2Snapshot, Qcow2SnapshotView, Qcow2Validation};

mod physical_validation;
pub use physical_validation::{
    PhysicalFingerprint, PhysicalValidationBudget, PhysicalValidationLimitExceeded,
    PhysicalValidationLimits, PhysicalValidationResource, PhysicalValidationUsage,
};
#[cfg(feature = "std")]
mod raw_write;
#[cfg(feature = "std")]
pub use raw_write::RawWriter;
mod vdi;
pub use vdi::Vdi;
mod vmdk;
pub use vmdk::{Vmdk, VmdkExtentBinding};
#[cfg(feature = "std")]
mod image;
#[cfg(feature = "std")]
mod image_writer;
#[cfg(feature = "std")]
mod operation;
#[cfg(feature = "std")]
pub use image::{
    Image, ImageInfo, ShrinkPolicy, compare_images, compare_images_with_context, convert_image,
    convert_image_with_context, copy_image, copy_image_with_cancel, copy_image_with_context,
    hash_image, hash_image_with_context, resize_image, resize_image_with_context,
};
#[cfg(feature = "std")]
pub use image_writer::ImageWriter;
#[cfg(feature = "std")]
pub use operation::OperationError;
#[cfg(feature = "std")]
mod operation_context;
#[cfg(feature = "std")]
pub use operation_context::{
    OperationCancelled, OperationContext, OperationLimitExceeded, OperationLimits, OperationPhase,
    OperationProgress, OperationResource, OperationUsage,
};
#[cfg(feature = "std")]
mod writer_open;
#[cfg(feature = "std")]
pub use writer_open::{RecoveryPolicy, RecoveryRequired, WriterOpenOptions};
#[cfg(feature = "std")]
mod reader_open;
#[cfg(feature = "std")]
pub use reader_open::{ReadRecoveryPolicy, ReaderOpenOptions};
#[cfg(feature = "std")]
mod export_source;
#[cfg(feature = "std")]
mod qcow2_write;
#[cfg(feature = "std")]
pub use qcow2_write::{create_qcow2, create_sparse_qcow2};
#[cfg(feature = "std")]
mod qcow2_writer;
#[cfg(feature = "std")]
pub use qcow2_writer::Qcow2Writer;
#[cfg(feature = "std")]
mod qcow2_overlay;
#[cfg(feature = "std")]
pub use qcow2_overlay::{create_qcow2_overlay, create_qcow2_overlay_with_chain};
#[cfg(feature = "std")]
mod graph;
#[cfg(feature = "std")]
mod graph_manifest;
#[cfg(feature = "std")]
pub use graph::{ImageGraph, ImageSpec};
#[cfg(feature = "std")]
pub use graph_manifest::GraphManifest;
#[cfg(feature = "std")]
mod compact;
#[cfg(feature = "std")]
pub use compact::{compact_image, compact_image_with_cancel, compact_image_with_context};
#[cfg(feature = "std")]
mod check;
#[cfg(feature = "std")]
pub use check::{
    CheckOptions, CheckReport, CheckScope, check_image, check_image_with_cancel,
    check_image_with_context, check_image_with_limits, check_image_with_limits_and_context,
    check_payload_with_cancel, check_payload_with_context,
};
#[cfg(feature = "std")]
mod vdi_write;
#[cfg(feature = "std")]
pub use vdi_write::{create_vdi, create_vdi_overlay};
#[cfg(feature = "std")]
mod vdi_writer;
#[cfg(feature = "std")]
pub use vdi_writer::VdiWriter;
mod vhdx;
pub use vhdx::Vhdx;
#[cfg(feature = "std")]
mod vhdx_write;
#[cfg(feature = "std")]
pub use vhdx_write::{create_vhdx, create_vhdx_overlay};
#[cfg(feature = "std")]
mod vhdx_writer;
#[cfg(feature = "std")]
pub use vhdx_writer::VhdxWriter;
#[cfg(feature = "std")]
mod vhdx_recover;
#[cfg(feature = "std")]
pub use vhdx_recover::{recover_vhdx, recover_vhdx_chain};
#[cfg(feature = "std")]
mod vmdk_write;
#[cfg(feature = "std")]
pub use vmdk_write::create_vmdk;
#[cfg(feature = "std")]
mod vmdk_writer;
#[cfg(feature = "std")]
pub use vmdk_writer::VmdkWriter;
#[cfg(feature = "std")]
mod write;
#[cfg(feature = "std")]
pub use write::{DiscardPolicy, DiscardResult, WriteAt, zero_image_with_context};
#[cfg(feature = "std")]
mod info;
#[cfg(feature = "std")]
pub use info::{
    Capability, DiskGeometry, ImageCapabilities, ImageInspection, ImageOperation, ImageProfile,
    InspectImage, UnsupportedReason, ValidationLevel,
};

/// A raw image opened read-only and retained through all deferred reads.
///
/// The private file cursor is serialized for portable exact reads. This does
/// not lock out external writers or provide a snapshot. Use an immutable source.
#[cfg(feature = "std")]
pub struct RawDisk {
    file: Mutex<File>,
    host: HostSourceContext,
    length: u64,
    context: ReadContext,
}

#[cfg(feature = "std")]
impl RawDisk {
    pub(crate) fn identity(&self) -> io::Result<same_file::Handle> {
        let file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("disk reader mutex poisoned"))?;
        Ok(same_file::Handle::from_file(file.try_clone()?)?)
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
            host: HostSourceContext::new(path.to_path_buf(), &file)?,
            file: Mutex::new(file),
            context: ReadContext {
                container: Some(path.to_string_lossy().into_owned()),
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
            .map_err(Into::into)
    }
    /// Open a regular raw image. Device files and directories are rejected.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let context = ReadContext {
            container: Some(path.to_string_lossy().into_owned()),
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
            host: HostSourceContext::new(path.canonicalize()?, &file)?,
            file: Mutex::new(file),
            length: metadata.len(),
            context: ReadContext {
                container: Some(path.canonicalize()?.to_string_lossy().into_owned()),
                ..context
            },
        })
    }
}

#[cfg(feature = "std")]
impl ReadAt for RawDisk {
    fn host_context(&self) -> Option<&dyn core::any::Any> {
        Some(&self.host)
    }
    fn source_identity(&self) -> Option<SourceIdentity> {
        Some(self.host.token(self.length))
    }
    fn context(&self) -> ReadContext {
        self.context.clone()
    }
    fn len(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        let result: io::Result<()> = (|| {
            use std::io::{Read, Seek, SeekFrom};
            check_range(offset, destination.len() as u64, self.length)?;
            let mut file = self
                .file
                .lock()
                .map_err(|_| io::Error::other("disk reader mutex poisoned"))?;
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(destination).map_err(Into::into)
        })();
        result.map_err(|e| {
            let mut context = self.context();
            context.offset = Some(offset);
            context.error("read raw image", e)
        })
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use std::{io::Write, sync::Arc};

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
