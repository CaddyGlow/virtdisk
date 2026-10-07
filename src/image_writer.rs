//! Explicit format-dispatched mutable access with retained writer locks.
use crate::{
    DiscardPolicy, DiscardResult, ImageFormat, ImageInspection, InspectImage, Qcow2Writer, RawDisk,
    RawWriter, ReadAt, ShrinkPolicy, VdiWriter, VhdxWriter, VmdkWriter, WriteAt,
};
use std::{
    io,
    path::{Path, PathBuf},
};
enum Kind {
    Raw(RawWriter),
    Qcow2(Qcow2Writer),
    Vdi(VdiWriter),
    Vmdk(VmdkWriter),
    Vhdx(Box<VhdxWriter>),
}
/// An explicitly selected offline writable image profile.
///
/// Retains the concrete writer's exclusive lock and recovery contract. Parents
/// must remain immutable. Native resize requires exclusive mutable access and
/// callers must exclude dependents and external readers. Mutable reads deliberately
/// do not implement the immutable `ReadAt` contract.
pub struct ImageWriter {
    kind: Kind,
}
impl ImageWriter {
    /// Delete one native QCOW2 disk snapshot while preserving other saved states.
    /// Uses the bounded standalone Linux lifecycle profile and redo recovery.
    /// Other families return `Unsupported` before mutation.
    pub fn delete_snapshot(&mut self, id: &[u8]) -> io::Result<crate::Qcow2Snapshot> {
        match &mut self.kind {
            Kind::Qcow2(writer) => writer.delete_snapshot(id),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native internal snapshot deletion is unavailable for this profile",
            )),
        }
    }
    /// Restore the active QCOW2 disk mapping from a retained native snapshot.
    /// The selected snapshot and its siblings remain saved. Callers must exclude
    /// external readers and own every dependent; saved capacity may change.
    /// Other families return `Unsupported` before mutation.
    pub fn revert_snapshot(&mut self, id: &[u8]) -> io::Result<crate::Qcow2Snapshot> {
        match &mut self.kind {
            Kind::Qcow2(writer) => writer.revert_snapshot(id),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native internal snapshot revert is unavailable for this profile",
            )),
        }
    }
    /// Create a native disk-only QCOW2 internal snapshot under the retained lock.
    /// Requires the bounded standalone Linux profile described by
    /// [`Qcow2Writer::create_snapshot`]. Other families return `Unsupported`;
    /// external derived images use [`crate::ImageGraph`] instead.
    pub fn create_snapshot(&mut self, id: &[u8], name: &[u8]) -> io::Result<crate::Qcow2Snapshot> {
        match &mut self.kind {
            Kind::Qcow2(writer) => writer.create_snapshot(id, name),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native internal snapshot creation is unavailable for this profile",
            )),
        }
    }
    /// Create a zero-readable image with unallocated payload mappings where supported.
    /// Raw and VHDX use their existing sparse creation behavior. QCOW2, VDI and
    /// hosted VMDK require Linux for later journaled allocation. Does not promise
    /// host physical sparseness. Retains the writer lock and never overwrites.
    pub fn create_sparse(
        path: impl AsRef<Path>,
        format: ImageFormat,
        size: u64,
    ) -> io::Result<Self> {
        let path = path.as_ref();
        let kind = match format {
            ImageFormat::Raw => Kind::Raw(RawWriter::create(path, size)?),
            ImageFormat::Qcow2 => Kind::Qcow2(Qcow2Writer::create_sparse(path, size)?),
            ImageFormat::Vdi => Kind::Vdi(VdiWriter::create_sparse(path, size)?),
            ImageFormat::Vmdk => Kind::Vmdk(VmdkWriter::create_sparse(path, size)?),
            ImageFormat::Vhdx => Kind::Vhdx(Box::new(VhdxWriter::create(path, size)?)),
        };
        Ok(Self { kind })
    }
    /// Create a zero-readable standalone image while retaining its exclusive lock.
    ///
    /// Uses each family's existing initial profile: raw host-dependent sparse
    /// storage, allocated QCOW2/VDI/hosted VMDK payload, or sparse dynamic VHDX.
    /// This does not promise host preallocation. Container capacities must satisfy
    /// their sector alignment and metadata limits. Existing paths are never
    /// overwritten. Failure can leave a partial output; creation does not sync
    /// the parent directory. Call `flush` to request durability before closing.
    pub fn create(path: impl AsRef<Path>, format: ImageFormat, size: u64) -> io::Result<Self> {
        let path = path.as_ref();
        let kind = match format {
            ImageFormat::Raw => Kind::Raw(RawWriter::create(path, size)?),
            ImageFormat::Qcow2 => Kind::Qcow2(Qcow2Writer::create(path, size)?),
            ImageFormat::Vdi => Kind::Vdi(VdiWriter::create(path, size)?),
            ImageFormat::Vmdk => Kind::Vmdk(VmdkWriter::create(path, size)?),
            ImageFormat::Vhdx => Kind::Vhdx(Box::new(VhdxWriter::create(path, size)?)),
        };
        Ok(Self { kind })
    }
    /// Open a supported standalone writable profile without authorizing embedded files.
    pub fn open(path: impl AsRef<Path>, format: ImageFormat) -> io::Result<Self> {
        let path = path.as_ref();
        let kind = match format {
            ImageFormat::Raw => Kind::Raw(RawWriter::open(path)?),
            ImageFormat::Qcow2 => Kind::Qcow2(Qcow2Writer::open(path)?),
            ImageFormat::Vdi => Kind::Vdi(VdiWriter::open(path)?),
            ImageFormat::Vmdk => Kind::Vmdk(VmdkWriter::open(path)?),
            ImageFormat::Vhdx => Kind::Vhdx(Box::new(VhdxWriter::open(path)?)),
        };
        Ok(Self { kind })
    }
    /// Open a writable child with explicitly authorized immutable parent/extent files.
    /// VDI requires ordered direct-parent through base paths because it stores UUIDs
    /// rather than filenames. Other families resolve only explicitly listed files.
    pub fn open_chain(
        path: impl AsRef<Path>,
        format: ImageFormat,
        authorized_paths: &[PathBuf],
    ) -> io::Result<Self> {
        let path = path.as_ref();
        let kind = match format {
            ImageFormat::Raw => Kind::Raw(RawWriter::open(path)?),
            ImageFormat::Qcow2 => Kind::Qcow2(Qcow2Writer::open_chain(path, authorized_paths)?),
            ImageFormat::Vdi => Kind::Vdi(VdiWriter::open_chain(path, authorized_paths)?),
            ImageFormat::Vmdk => {
                let source = RawDisk::open(path)?;
                let mut magic = [0; 4];
                if source.len() >= 4 {
                    source.read_exact_at(0, &mut magic)?;
                }
                drop(source);
                if magic == *b"KDMV" {
                    Kind::Vmdk(VmdkWriter::open_chain(path, authorized_paths)?)
                } else {
                    Kind::Vmdk(VmdkWriter::open_descriptor(path, authorized_paths)?)
                }
            }
            ImageFormat::Vhdx => {
                Kind::Vhdx(Box::new(VhdxWriter::open_chain(path, authorized_paths)?))
            }
        };
        Ok(Self { kind })
    }
    fn writer(&self) -> &dyn WriteAt {
        match &self.kind {
            Kind::Raw(writer) => writer,
            Kind::Qcow2(writer) => writer,
            Kind::Vdi(writer) => writer,
            Kind::Vmdk(writer) => writer,
            Kind::Vhdx(writer) => writer.as_ref(),
        }
    }
    /// Request host preallocation of a bounded raw range without changing bytes.
    /// Other container profiles return `Unsupported` rather than zeroing or
    /// expanding their payload mappings. Host platform support and durability
    /// follow [`RawWriter::preallocate`].
    pub fn preallocate(&self, offset: u64, length: u64) -> io::Result<()> {
        crate::check_range(offset, length, self.writer().len())?;
        match &self.kind {
            Kind::Raw(writer) => writer.preallocate(offset, length),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "container preallocation is unavailable",
            )),
        }
    }
    /// Change capacity in a supported native writable profile.
    ///
    /// Shrink requires an explicit tail policy. Zero-tail validation does not
    /// establish guest filesystem safety. Callers must own every dependent and
    /// exclude external readers/views for the operation. Raw resize follows host
    /// truncation semantics; containers use their documented recovery protocols.
    /// Unsupported profiles can use `resize_image` for a new-output rewrite.
    pub fn resize(&mut self, new_size: u64, policy: ShrinkPolicy) -> io::Result<()> {
        match &mut self.kind {
            Kind::Raw(writer) => {
                let old_size = writer.len();
                if new_size < old_size {
                    match policy {
                        ShrinkPolicy::Reject => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "shrinking requires an explicit tail policy",
                            ));
                        }
                        ShrinkPolicy::RequireZero => {
                            let mut offset = new_size;
                            let mut buffer = [0; 65536];
                            while offset < old_size {
                                let count = (old_size - offset).min(buffer.len() as u64) as usize;
                                writer.read_exact_at(offset, &mut buffer[..count])?;
                                if buffer[..count].iter().any(|byte| *byte != 0) {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidInput,
                                        "removed tail contains nonzero content",
                                    ));
                                }
                                offset += count as u64;
                            }
                        }
                        ShrinkPolicy::AllowDataLoss => {}
                    }
                }
                writer.resize(new_size)
            }
            Kind::Qcow2(writer) => writer.resize(new_size, policy),
            Kind::Vdi(writer) => writer.resize(new_size, policy),
            Kind::Vmdk(writer) => writer.resize(new_size, policy),
            Kind::Vhdx(writer) => writer.resize(new_size, policy),
        }
    }
    /// Read current logical bytes under the concrete writer's synchronization.
    /// I/O errors can partially modify the destination.
    pub fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        match &self.kind {
            Kind::Raw(writer) => writer.read_exact_at(offset, destination),
            Kind::Qcow2(writer) => writer.read_exact_at(offset, destination),
            Kind::Vdi(writer) => writer.read_exact_at(offset, destination),
            Kind::Vmdk(writer) => writer.read_exact_at(offset, destination),
            Kind::Vhdx(writer) => writer.read_exact_at(offset, destination),
        }
    }
}
impl WriteAt for ImageWriter {
    fn len(&self) -> u64 {
        self.writer().len()
    }
    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.writer().write_all_at(offset, data)
    }
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        self.writer().write_zeroes(offset, length)
    }
    fn flush(&self) -> io::Result<()> {
        self.writer().flush()
    }
    fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
    ) -> io::Result<DiscardResult> {
        self.writer().discard(offset, length, policy)
    }
}
impl InspectImage for ImageWriter {
    fn inspection(&self) -> ImageInspection {
        match &self.kind {
            Kind::Raw(writer) => writer.inspection(),
            Kind::Qcow2(writer) => writer.inspection(),
            Kind::Vdi(writer) => writer.inspection(),
            Kind::Vmdk(writer) => writer.inspection(),
            Kind::Vhdx(writer) => writer.inspection(),
        }
    }
}
