//! Bounded read-only virtual disk access: raw images and QCOW2 chains.
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
pub use policy::{
    CacheReservation, ParserLimits, ReadBudget, ReadBudgetUsage, ReadContext, ReadError,
    contextual_reader,
};

mod qcow2;
pub use qcow2::{Qcow2, Qcow2Validation};

/// An exact positional reader with a fixed logical length.
///
/// Implementations must reject overflowing/out-of-bounds ranges, including an
/// empty read beyond the end. Reads may partially modify the destination on I/O
/// failure; callers must discard it on error. Implementations must be safe to
/// share across threads without a shared seek cursor affecting results.
pub trait ReadAt: Send + Sync {
    /// Logical length in bytes, fixed for this reader's lifetime.
    fn len(&self) -> u64;

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
