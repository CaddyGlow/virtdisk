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
///
/// Failed operations on an existing handle carry [`crate::OperationError`] in
/// `io::Error::get_ref()`, preserving the underlying error kind and source.
/// Construction and opening retain their existing concrete error contracts.
/// Error context does not imply rollback; recovery requirements remain specific
/// to the opened profile.
pub struct ImageWriter {
    kind: Kind,
}
impl ImageWriter {
    /// Open with explicitly selected recovery and dependency authorization policies.
    ///
    /// Defaults to standalone opening with pending recovery forbidden. Select
    /// [`crate::RecoveryPolicy::Recover`] to permit supported redo under retained
    /// locks. An unsuccessful recovering open may partially replay metadata;
    /// retry recovery before normal access. Existing `open`/`open_chain` methods
    /// retain their per-format recovery behavior. See [`crate::WriterOpenOptions`].
    pub fn open_with_options(
        path: impl AsRef<Path>,
        format: ImageFormat,
        options: &crate::WriterOpenOptions,
    ) -> io::Result<Self> {
        let path = path.as_ref();
        let authorized = options.authorized.as_deref();
        if format == ImageFormat::Raw && authorized.is_some_and(|paths| !paths.is_empty()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "raw writers do not accept authorized dependency paths",
            ));
        }
        let kind = match format {
            ImageFormat::Raw => Kind::Raw(RawWriter::open(path)?),
            ImageFormat::Qcow2 => Kind::Qcow2(Qcow2Writer::open_policy(
                path,
                authorized.unwrap_or_default(),
                options.recovery,
            )?),
            ImageFormat::Vdi => Kind::Vdi(VdiWriter::open_policy(
                path,
                authorized.unwrap_or_default(),
                options.recovery,
            )?),
            ImageFormat::Vmdk => {
                Kind::Vmdk(VmdkWriter::open_policy(path, authorized, options.recovery)?)
            }
            ImageFormat::Vhdx => Kind::Vhdx(Box::new(VhdxWriter::open_policy(
                path,
                authorized,
                options.recovery,
            )?)),
        };
        Ok(Self { kind })
    }
    fn contextual<T>(
        &self,
        operation: crate::ImageOperation,
        range: Option<(u64, u64)>,
        result: io::Result<T>,
    ) -> io::Result<T> {
        let format = match &self.kind {
            Kind::Raw(_) => ImageFormat::Raw,
            Kind::Qcow2(_) => ImageFormat::Qcow2,
            Kind::Vdi(_) => ImageFormat::Vdi,
            Kind::Vmdk(_) => ImageFormat::Vmdk,
            Kind::Vhdx(_) => ImageFormat::Vhdx,
        };
        result.map_err(|error| crate::OperationError::wrap(operation, format, range, error))
    }
    fn native_snapshot_with_context(
        &mut self,
        operation: crate::ImageOperation,
        phase: crate::OperationPhase,
        context: &mut crate::OperationContext<'_>,
        mutate: impl FnOnce(&mut Self) -> io::Result<crate::Qcow2Snapshot>,
    ) -> io::Result<crate::Qcow2Snapshot> {
        self.contextual(operation, None, context.preflight_io(1))?;
        self.contextual(operation, None, context.observe_phase(phase, 0, 0))?;
        context.attempted_io();
        mutate(self)
    }
    /// Create a native disk snapshot with cancellation before its transaction.
    ///
    /// Counts one attempted native call, zero logical bytes and no facade scratch.
    /// Metadata, journal I/O and backend allocations are outside accounting.
    /// No callback runs during or after mutation; flush remains explicit. Backend
    /// errors retain their existing recovery effects. See [`Self::create_snapshot`].
    pub fn create_snapshot_with_context(
        &mut self,
        id: &[u8],
        name: &[u8],
        context: &mut crate::OperationContext<'_>,
    ) -> io::Result<crate::Qcow2Snapshot> {
        self.native_snapshot_with_context(
            crate::ImageOperation::NativeSnapshotCreate,
            crate::OperationPhase::NativeSnapshotCreation,
            context,
            |writer| writer.create_snapshot(id, name),
        )
    }
    /// Delete a native snapshot with cancellation before its transaction.
    /// Uses the accounting and callback contract of [`Self::create_snapshot_with_context`].
    /// See [`Self::delete_snapshot`] for supported profiles and saved-state preservation.
    pub fn delete_snapshot_with_context(
        &mut self,
        id: &[u8],
        context: &mut crate::OperationContext<'_>,
    ) -> io::Result<crate::Qcow2Snapshot> {
        self.native_snapshot_with_context(
            crate::ImageOperation::NativeSnapshotDelete,
            crate::OperationPhase::NativeSnapshotDeletion,
            context,
            |writer| writer.delete_snapshot(id),
        )
    }
    /// Restore a native snapshot with cancellation before its transaction.
    /// Uses the accounting and callback contract of [`Self::create_snapshot_with_context`].
    /// See [`Self::revert_snapshot`] for dependency exclusion and capacity changes.
    pub fn revert_snapshot_with_context(
        &mut self,
        id: &[u8],
        context: &mut crate::OperationContext<'_>,
    ) -> io::Result<crate::Qcow2Snapshot> {
        self.native_snapshot_with_context(
            crate::ImageOperation::NativeSnapshotRevert,
            crate::OperationPhase::NativeSnapshotRevert,
            context,
            |writer| writer.revert_snapshot(id),
        )
    }

    /// Delete one native QCOW2 disk snapshot while preserving other saved states.
    /// Uses the bounded Linux lifecycle profile with authorized immutable parents and redo recovery.
    /// Other families return `Unsupported` before mutation.
    pub fn delete_snapshot(&mut self, id: &[u8]) -> io::Result<crate::Qcow2Snapshot> {
        let result = match &mut self.kind {
            Kind::Qcow2(writer) => writer.delete_snapshot(id),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native internal snapshot deletion is unavailable for this profile",
            )),
        };
        self.contextual(crate::ImageOperation::NativeSnapshotDelete, None, result)
    }
    /// Restore the active QCOW2 disk mapping from a retained native snapshot.
    /// The selected snapshot and its siblings remain saved. Callers must exclude
    /// external readers and own every dependent; saved capacity may change.
    /// Other families return `Unsupported` before mutation.
    pub fn revert_snapshot(&mut self, id: &[u8]) -> io::Result<crate::Qcow2Snapshot> {
        let result = match &mut self.kind {
            Kind::Qcow2(writer) => writer.revert_snapshot(id),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native internal snapshot revert is unavailable for this profile",
            )),
        };
        self.contextual(crate::ImageOperation::NativeSnapshotRevert, None, result)
    }
    /// Create a native disk-only QCOW2 internal snapshot under the retained lock.
    /// Requires the bounded Linux profile with authorized immutable parents described by
    /// [`Qcow2Writer::create_snapshot`]. Other families return `Unsupported`;
    /// external derived images use [`crate::ImageGraph`] instead.
    pub fn create_snapshot(&mut self, id: &[u8], name: &[u8]) -> io::Result<crate::Qcow2Snapshot> {
        let result = match &mut self.kind {
            Kind::Qcow2(writer) => writer.create_snapshot(id, name),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native internal snapshot creation is unavailable for this profile",
            )),
        };
        self.contextual(crate::ImageOperation::NativeSnapshotCreate, None, result)
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
                if magic == *b"KDMV" || VmdkWriter::split_sparse_profile(path)? {
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
        let result = (|| {
            crate::check_range(offset, length, self.writer().len())?;
            match &self.kind {
                Kind::Raw(writer) => writer.preallocate(offset, length),
                _ => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "container preallocation is unavailable",
                )),
            }
        })();
        self.contextual(
            crate::ImageOperation::Preallocate,
            Some((offset, length)),
            result,
        )
    }
    fn native_storage_with_context<T>(
        &self,
        range: (u64, u64),
        operation: crate::ImageOperation,
        phase: crate::OperationPhase,
        context: &mut crate::OperationContext<'_>,
        mutate: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let (offset, length) = range;
        self.contextual(
            operation,
            Some(range),
            crate::check_range(offset, length, self.len()),
        )?;
        self.contextual(
            operation,
            Some(range),
            context.preflight_chunks(length, length.max(1), 1),
        )?;
        self.contextual(operation, Some(range), context.preflight_io(1))?;
        self.contextual(
            operation,
            Some(range),
            context.observe_phase(phase, 0, length),
        )?;
        context.attempted_io();
        let result = mutate()?;
        context.completed(length);
        Ok(result)
    }
    /// Preallocate a range with quotas and cancellation before native dispatch.
    ///
    /// Counts the successful requested logical range and one attempted facade call,
    /// including failed native calls. Uses no facade scratch. Backend allocation,
    /// metadata, synchronization and any partial effects on error remain outside
    /// accounting. No callback runs during or after mutation; flush is explicit.
    /// See [`Self::preallocate`] for native support and content preservation.
    pub fn preallocate_with_context(
        &self,
        offset: u64,
        length: u64,
        context: &mut crate::OperationContext<'_>,
    ) -> io::Result<()> {
        self.native_storage_with_context(
            (offset, length),
            crate::ImageOperation::Preallocate,
            crate::OperationPhase::NativePreallocation,
            context,
            || self.preallocate(offset, length),
        )
    }
    /// Discard a range with quotas and cancellation before native dispatch.
    ///
    /// Retains native alignment, reclamation and explicit fallback behavior.
    /// Counts the successful requested logical range and one attempted facade call;
    /// no facade scratch or implicit flush. Backend I/O, journal/allocation work
    /// and partial effects on failure remain outside accounting. No callback runs
    /// during or after mutation. See [`WriteAt::discard`] for failure semantics.
    pub fn discard_with_context(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
        context: &mut crate::OperationContext<'_>,
    ) -> io::Result<DiscardResult> {
        self.native_storage_with_context(
            (offset, length),
            crate::ImageOperation::Discard,
            crate::OperationPhase::NativeDiscard,
            context,
            || self.discard(offset, length, policy),
        )
    }

    /// Change capacity in a supported native writable profile.
    ///
    /// Shrink requires an explicit tail policy. Zero-tail validation does not
    /// establish guest filesystem safety. Callers must own every dependent and
    /// exclude external readers/views for the operation. Raw resize follows host
    /// truncation semantics; containers use their documented recovery protocols.
    /// Unsupported profiles can use `resize_image` for a new-output rewrite.
    pub fn resize(&mut self, new_size: u64, policy: ShrinkPolicy) -> io::Result<()> {
        let result = self.resize_inner(new_size, policy);
        self.contextual(crate::ImageOperation::Resize, None, result)
    }
    fn resize_inner(&mut self, new_size: u64, policy: ShrinkPolicy) -> io::Result<()> {
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
    /// Resize with bounded zero-tail validation and cancellation before mutation.
    ///
    /// `RequireZero` scans the resolved removed tail in bounded chunks under the
    /// retained locks. Byte/I/O quotas include the scan and one attempted native
    /// resize call; growth and explicit data-loss shrink consume no logical bytes.
    /// The final `NativeResize` callback precedes the native transaction. No
    /// callback interrupts that transaction or follows it. Backend metadata,
    /// journal I/O and allocations are outside context accounting. Flush remains
    /// explicit; backend errors retain their format-specific recovery effects.
    /// The caller must exclude external readers, dependents and parent mutation.
    pub fn resize_with_context(
        &mut self,
        new_size: u64,
        policy: ShrinkPolicy,
        context: &mut crate::OperationContext<'_>,
    ) -> io::Result<()> {
        let result = (|| {
            let old_size = self.len();
            if new_size < old_size && policy == ShrinkPolicy::Reject {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "shrinking requires an explicit tail policy",
                ));
            }
            let tail = if new_size < old_size && policy == ShrinkPolicy::RequireZero {
                old_size - new_size
            } else {
                0
            };
            let scratch = context.preflight(tail, 1, 1)?;
            let scan_calls = if tail == 0 {
                0
            } else {
                u128::from(tail.div_ceil(scratch as u64))
            };
            context.preflight_io(scan_calls + 1)?;
            if tail != 0 {
                context.observe_phase(crate::OperationPhase::TailValidation, 0, tail)?;
                let mut buffer = crate::operation_context::scratch_buffer(scratch)?;
                context.scratch(scratch);
                let mut offset = new_size;
                while offset < old_size {
                    let count = (old_size - offset).min(buffer.len() as u64) as usize;
                    context.attempted_io();
                    self.read_exact_at(offset, &mut buffer[..count])?;
                    let nonzero = buffer[..count].iter().any(|byte| *byte != 0);
                    offset += count as u64;
                    context.completed(count as u64);
                    context.observe_phase(
                        crate::OperationPhase::TailValidation,
                        offset - new_size,
                        tail,
                    )?;
                    if nonzero {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "removed tail contains nonzero content",
                        ));
                    }
                }
            }
            context.observe_phase(crate::OperationPhase::NativeResize, 0, 0)?;
            context.attempted_io();
            // The zero-tail policy has been verified under exclusive mutable
            // access. Avoid scanning the same bytes again inside the backend.
            self.resize_inner(
                new_size,
                if tail != 0 {
                    ShrinkPolicy::AllowDataLoss
                } else {
                    policy
                },
            )
        })();
        self.contextual(crate::ImageOperation::Resize, None, result)
    }

    /// Read current logical bytes under the concrete writer's synchronization.
    /// I/O errors can partially modify the destination.
    pub fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        let result = match &self.kind {
            Kind::Raw(writer) => writer.read_exact_at(offset, destination),
            Kind::Qcow2(writer) => writer.read_exact_at(offset, destination),
            Kind::Vdi(writer) => writer.read_exact_at(offset, destination),
            Kind::Vmdk(writer) => writer.read_exact_at(offset, destination),
            Kind::Vhdx(writer) => writer.read_exact_at(offset, destination),
        };
        self.contextual(
            crate::ImageOperation::Read,
            Some((offset, destination.len() as u64)),
            result,
        )
    }
}
impl WriteAt for ImageWriter {
    fn len(&self) -> u64 {
        self.writer().len()
    }
    fn write_all_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.contextual(
            crate::ImageOperation::Write,
            Some((offset, data.len() as u64)),
            self.writer().write_all_at(offset, data),
        )
    }
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        self.contextual(
            crate::ImageOperation::WriteZeroes,
            Some((offset, length)),
            self.writer().write_zeroes(offset, length),
        )
    }
    fn flush(&self) -> io::Result<()> {
        self.contextual(crate::ImageOperation::Flush, None, self.writer().flush())
    }
    fn discard(
        &self,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
    ) -> io::Result<DiscardResult> {
        self.contextual(
            crate::ImageOperation::Discard,
            Some((offset, length)),
            self.writer().discard(offset, length, policy),
        )
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
