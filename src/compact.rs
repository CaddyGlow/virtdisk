//! Logical-content-preserving sparse rewrites.
use crate::{Image, ImageFormat, Qcow2, RawDisk, RawWriter, ReadAt};
use std::{io, path::Path, sync::Arc};

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
    compact_image_with_cancel(source, output, format, &|| false)
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
    let source: &dyn ReadAt = &proxy;
    crate::image::publish_image(output.as_ref(), |temporary| {
        match format {
            ImageFormat::Raw => {
                let writer = RawWriter::create(temporary, source.len())?;
                let mut buffer = vec![0; 65536];
                let mut offset = 0;
                while offset < source.len() {
                    let count = (source.len() - offset).min(buffer.len() as u64) as usize;
                    source.read_exact_at(offset, &mut buffer[..count])?;
                    if buffer[..count].iter().any(|b| *b != 0) {
                        writer.write_all_at(offset, &buffer[..count])?;
                    }
                    offset += count as u64;
                }
                writer.flush()?;
            }
            ImageFormat::Qcow2 => crate::create_sparse_qcow2(temporary, source)?,
            ImageFormat::Vdi => crate::create_vdi(temporary, source)?,
            ImageFormat::Vmdk => crate::create_vmdk(temporary, source)?,
            ImageFormat::Vhdx => crate::create_vhdx(temporary, source)?,
        }
        let reopened = Image::open(temporary, Some(format))?;
        if !crate::compare_images(source, &reopened)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "compacted image differs from source",
            ));
        }
        if format == ImageFormat::Qcow2 {
            Qcow2::open(Arc::new(RawDisk::open(temporary)?))?.validate_active_mapping()?;
        }
        proxy.check()?;
        Ok(())
    })
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
