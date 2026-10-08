//! Bounded format detection and logical image operations.
use crate::ImageFormat;
use crate::detect_format;
use crate::io;
use crate::{
    ImageInspection, InspectImage, Qcow2, RawDisk, RawWriter, ReadAt, ReadBudget, ReadContext, Vdi,
    Vhdx, Vmdk, WriteAt,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Basic inspection facts for an opened image, without implied deep validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// Selected container family.
    pub format: ImageFormat,
    /// Addressable logical disk bytes.
    pub virtual_size: u64,
    /// Container file length, not filesystem allocated storage.
    pub container_size: u64,
}

/// A format-dispatched immutable image reader.
///
/// The caller must keep source files immutable throughout all reads. This
/// ordinary [`Self::open`] constructor never authorizes embedded parents or
/// additional extent files. [`Self::open_chain`] requires explicit authorization.
pub struct Image {
    reader: Arc<dyn ReadAt>,
    info: ImageInfo,
    inspection: ImageInspection,
}

impl Image {
    /// Open with explicit format, dependency authorization and caller parser limits.
    ///
    /// Recognition has its own fixed 68-byte bound; parser ceilings cover parsing,
    /// authorized chains and deferred reads. Unrecognized data requires explicit
    /// raw selection; parsing never falls back to raw. VHDX log replay is an
    /// explicitly selected immutable overlay, never a native file update.
    /// All sources must remain immutable for the reader's lifetime. Existing
    /// `open` and `open_chain` retain their established contracts.
    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: &crate::ReaderOpenOptions,
    ) -> io::Result<Self> {
        let path = path.as_ref();
        let source = Arc::new(RawDisk::open(path)?);
        let container_size = source.len();
        let format = match options.format {
            Some(format) => format,
            None => detect_format(source.as_ref())?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unrecognized image; select raw explicitly",
                )
            })?,
        };
        let replay = options.recovery == crate::ReadRecoveryPolicy::ReplayVhdxLog;
        if replay && format != ImageFormat::Vhdx {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "immutable log replay requires VHDX",
            ));
        }
        macro_rules! opened {
            ($image:expr) => {{
                let image = $image;
                let inspection = image.inspection();
                (Arc::new(image) as Arc<dyn ReadAt>, inspection)
            }};
        }
        let limits = options.limits;
        let (reader, mut inspection): (Arc<dyn ReadAt>, ImageInspection) =
            match (format, options.authorized.as_deref()) {
                (ImageFormat::Raw, paths) => {
                    if paths.is_some_and(|paths| !paths.is_empty()) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "raw image has no parent chain",
                        ));
                    }
                    let inspection = source.inspection();
                    (ReadBudget::new(limits)?.reader(source), inspection)
                }
                (ImageFormat::Qcow2, None) => opened!(Qcow2::open_with_limits(source, limits)?),
                (ImageFormat::Vdi, None) => opened!(Vdi::open_with_limits(source, limits)?),
                (ImageFormat::Vmdk, None) => opened!(Vmdk::open_with_limits(source, limits)?),
                (ImageFormat::Vhdx, None) if replay => {
                    opened!(Vhdx::open_recovered_with_limits(source, limits)?)
                }
                (ImageFormat::Vhdx, None) => opened!(Vhdx::open_with_limits(source, limits)?),
                (ImageFormat::Qcow2, Some(paths)) => {
                    opened!(Qcow2::open_chain_with_limits(path, paths, limits)?)
                }
                (ImageFormat::Vdi, Some(paths)) => {
                    opened!(Vdi::open_chain_with_limits(path, paths, limits)?)
                }
                (ImageFormat::Vmdk, Some(paths)) => {
                    opened!(Vmdk::open_chain_with_limits(path, paths, limits)?)
                }
                (ImageFormat::Vhdx, Some(paths)) if replay => {
                    opened!(Vhdx::open_recovered_chain_with_limits(path, paths, limits)?)
                }
                (ImageFormat::Vhdx, Some(paths)) => {
                    opened!(Vhdx::open_chain_with_limits(path, paths, limits)?)
                }
            };
        if replay {
            // An immutable log overlay can extend its addressable source beyond
            // EOF. Inspection reports the actual child file, excluding parents.
            inspection.container_size = Some(container_size);
            inspection.container_set_size = Some(container_size);
        }
        Ok(Self {
            info: ImageInfo {
                format,
                virtual_size: reader.len(),
                container_size,
            },
            reader,
            inspection,
        })
    }
    /// Open an immutable image with only explicitly authorized parent paths.
    ///
    /// VDI paths must be ordered from direct parent through base. Other families
    /// resolve embedded references only within the supplied authorization set.
    /// Raw images reject parent authorization. All files must remain immutable.
    pub fn open_chain(
        path: impl AsRef<Path>,
        format: Option<ImageFormat>,
        authorized_paths: &[PathBuf],
    ) -> io::Result<Self> {
        let path = path.as_ref();
        let source = RawDisk::open(path)?;
        let container_size = source.len();
        let format = match format {
            Some(format) => Some(format),
            None => detect_format(&source)?,
        }
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "unrecognized image; select raw explicitly",
            )
        })?;
        drop(source);
        macro_rules! opened {
            ($image:expr) => {{
                let image = $image;
                let inspection = image.inspection();
                (Arc::new(image) as Arc<dyn ReadAt>, inspection)
            }};
        }
        let (reader, inspection) = match format {
            ImageFormat::Raw => {
                if !authorized_paths.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "raw image has no parent chain",
                    ));
                }
                opened!(RawDisk::open(path)?)
            }
            ImageFormat::Qcow2 => opened!(Qcow2::open_chain(path, authorized_paths)?),
            ImageFormat::Vdi => opened!(Vdi::open_chain(path, authorized_paths)?),
            ImageFormat::Vmdk => opened!(Vmdk::open_chain(path, authorized_paths)?),
            ImageFormat::Vhdx => opened!(Vhdx::open_chain(path, authorized_paths)?),
        };
        Ok(Self {
            info: ImageInfo {
                format,
                virtual_size: reader.len(),
                container_size,
            },
            reader,
            inspection,
        })
    }
    /// Open a regular image with bounded format recognition.
    ///
    /// Unrecognized data requires explicitly selecting raw. A recognized
    /// container is always parsed as that format unless raw is explicitly
    /// selected; parse failure never triggers raw fallback.
    pub fn open(path: impl AsRef<Path>, format: Option<ImageFormat>) -> io::Result<Self> {
        let source = Arc::new(RawDisk::open(path)?);
        let container_size = source.len();
        let format = match format {
            Some(format) => format,
            None => detect_format(source.as_ref())?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unrecognized image; select raw explicitly",
                )
            })?,
        };
        macro_rules! opened {
            ($image:expr) => {{
                let image = $image;
                let inspection = image.inspection();
                (Arc::new(image) as Arc<dyn ReadAt>, inspection)
            }};
        }
        let (reader, inspection): (Arc<dyn ReadAt>, ImageInspection) = match format {
            ImageFormat::Raw => {
                let inspection = source.inspection();
                (source, inspection)
            }
            ImageFormat::Qcow2 => opened!(Qcow2::open(source)?),
            ImageFormat::Vdi => opened!(Vdi::open(source)?),
            ImageFormat::Vmdk => opened!(Vmdk::open(source)?),
            ImageFormat::Vhdx => opened!(Vhdx::open(source)?),
        };
        let info = ImageInfo {
            format,
            virtual_size: reader.len(),
            container_size,
        };
        Ok(Self {
            reader,
            info,
            inspection,
        })
    }

    /// Inspect basic facts without claiming complete allocation validation.
    pub fn info(&self) -> &ImageInfo {
        &self.info
    }
}

impl InspectImage for Image {
    fn inspection(&self) -> ImageInspection {
        self.inspection
    }
}

impl ReadAt for Image {
    fn host_context(&self) -> Option<&dyn core::any::Any> {
        self.reader.host_context()
    }
    fn source_identity(&self) -> Option<crate::SourceIdentity> {
        self.reader.source_identity()
    }
    fn ancestor_identities(&self) -> Vec<crate::SourceIdentity> {
        self.reader.ancestor_identities()
    }
    fn visit_extents(
        &self,
        visitor: &mut dyn FnMut(crate::DiskExtent) -> io::Result<()>,
    ) -> io::Result<()> {
        self.reader.visit_extents(visitor)
    }
    fn len(&self) -> u64 {
        self.reader.len()
    }
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.reader.read_exact_at(offset, destination)
    }
    fn context(&self) -> ReadContext {
        self.reader.context()
    }
    fn budget(&self) -> Option<ReadBudget> {
        self.reader.budget()
    }
    fn sparse_holes(&self) -> io::Result<Vec<(u64, u64)>> {
        self.reader.sparse_holes()
    }
}

/// Compare logical lengths and bytes using bounded scratch memory.
///
/// Both inputs must remain immutable throughout the comparison. Allocation
/// layout and container metadata are not compared.
pub fn compare_images(left: &dyn ReadAt, right: &dyn ReadAt) -> io::Result<bool> {
    compare_images_with_context(left, right, &mut crate::OperationContext::default())
}
/// Compare logical bytes with cumulative budgets and cooperative progress.
///
/// Both sources must remain immutable. Combined scratch is bounded to 128 KiB.
/// Unequal capacities return false without callbacks or I/O. For equal sizes,
/// the complete request is preflighted; comparison stops after the first unequal
/// chunk, accounting that chunk as processed. Observers run before reads and
/// after compared chunks. Cancellation returns `Interrupted` even if all bytes
/// have been examined. No container metadata or allocation layout is compared.
pub fn compare_images_with_context(
    left: &dyn ReadAt,
    right: &dyn ReadAt,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<bool> {
    compare_images_in_phase(left, right, context, crate::OperationPhase::Processing)
}
pub(crate) fn compare_images_in_phase(
    left: &dyn ReadAt,
    right: &dyn ReadAt,
    context: &mut crate::OperationContext<'_>,
    phase: crate::OperationPhase,
) -> io::Result<bool> {
    let size = left.len();
    if size != right.len() {
        return Ok(false);
    }
    let scratch = context.preflight(size, 2, 2)?;
    context.observe_phase(phase, 0, size)?;
    if size == 0 {
        return Ok(true);
    }
    context.scratch(scratch * 2);
    let mut a = crate::operation_context::scratch_buffer(scratch)?;
    let mut b = crate::operation_context::scratch_buffer(scratch)?;
    let mut offset = 0;
    while offset < size {
        let count = (size - offset).min(scratch as u64) as usize;
        context.attempted_io();
        left.read_exact_at(offset, &mut a[..count])?;
        context.attempted_io();
        right.read_exact_at(offset, &mut b[..count])?;
        offset += count as u64;
        context.completed(count as u64);
        context.observe_phase(phase, offset, size)?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Hash immutable logical disk bytes with SHA-256 using bounded memory.
///
/// Container layout and metadata are not included. This is a content comparison
/// primitive, not evidence that the source was authenticated or remained stable.
pub fn hash_image(source: &dyn ReadAt) -> io::Result<[u8; 32]> {
    hash_image_with_context(source, &mut crate::OperationContext::default())
}
/// Hash immutable logical bytes with budgets and cooperative progress.
///
/// Preflights the complete request before callbacks, allocation or I/O. Scratch
/// is at most 64 KiB. Progress reports completed chunks; cancellation at any
/// boundary returns `Interrupted` without a digest. Underlying I/O errors are
/// preserved. Hashing does not authenticate the source or provide isolation.
pub fn hash_image_with_context(
    source: &dyn ReadAt,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<[u8; 32]> {
    let size = source.len();
    let scratch = context.preflight(size, 1, 1)?;
    context.observe(0, size)?;
    let mut hash = Sha256::new();
    context.scratch(scratch);
    let mut buffer = crate::operation_context::scratch_buffer(scratch)?;
    let mut offset = 0;
    while offset < size {
        let count = (size - offset).min(scratch as u64) as usize;
        context.attempted_io();
        source.read_exact_at(offset, &mut buffer[..count])?;
        hash.update(&buffer[..count]);
        offset += count as u64;
        context.completed(count as u64);
        context.observe(offset, size)?;
    }
    Ok(hash.finalize().into())
}

/// Materialize an immutable logical image into an equally sized raw writer.
///
/// Uses bounded scratch memory. Errors can leave a partially written output;
/// callers should use a disposable new output and publish only after validation.
/// The caller must flush explicitly to request durability.
pub fn copy_image(source: &dyn ReadAt, output: &dyn WriteAt) -> io::Result<()> {
    copy_image_with_cancel(source, output, || false)
}

/// Copy using cumulative caller budgets and cooperative progress.
///
/// The complete byte/I/O request is checked before callbacks, allocation or I/O.
/// Progress is reported before copying and after every completed chunk, including
/// the final partial chunk. An observer may cancel at any reported boundary;
/// even cancellation after the last chunk returns `Interrupted`. Completed bytes
/// remain in the output. Failed I/O may have additional partial effects.
/// Scratch is bounded to 64 KiB and allocated fallibly. No flush or publication
/// is implicit; exclude resize and keep the source immutable during copying.
pub fn copy_image_with_context(
    source: &dyn ReadAt,
    output: &dyn WriteAt,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<()> {
    let size = source.len();
    if size != output.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "image sizes differ",
        ));
    }
    let scratch = context.preflight(size, 1, 2)?;
    context.observe(0, size)?;
    if size == 0 {
        return Ok(());
    }
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(scratch)
        .map_err(io::Error::other)?;
    buffer.resize(scratch, 0);
    context.scratch(scratch);
    let mut offset = 0;
    while offset < size {
        let count = (size - offset).min(buffer.len() as u64) as usize;
        context.attempted_io();
        source.read_exact_at(offset, &mut buffer[..count])?;
        context.attempted_io();
        output.write_all_at(offset, &buffer[..count])?;
        offset += count as u64;
        context.completed(count as u64);
        context.observe(offset, size)?;
    }
    Ok(())
}

/// Copy logical bytes with cancellation checks before each bounded chunk.
///
/// Cancellation returns `Interrupted` and leaves already written chunks in the
/// output. No flush is implicit; use a disposable output for publication.
pub fn copy_image_with_cancel(
    source: &dyn ReadAt,
    output: &dyn WriteAt,
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<()> {
    let mut observer = |progress: crate::OperationProgress| {
        if progress.completed_bytes < progress.total_bytes && cancelled() {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    let mut context = crate::OperationContext::default().with_observer(&mut observer);
    copy_image_with_context(source, output, &mut context)
}

/// Convert immutable logical bytes and publish a new single-file image.
///
/// Writes into a private sibling staging directory, then publishes through a
/// hard link that never replaces an existing output. Requires host hard-link
/// support. On failure before publication the requested output is absent;
/// temporary-file cleanup is best effort. The source must remain immutable.
/// Payload is synced and logical bytes compared before publication. Comparison
/// can catch some source changes but does not provide snapshot isolation.
/// Parent-directory persistence is
/// requested on Unix; other platforms do not promise durable directory entries.
pub fn convert_image(
    source: &dyn ReadAt,
    output: impl AsRef<Path>,
    format: ImageFormat,
) -> io::Result<()> {
    convert_image_with_context(
        source,
        output,
        format,
        &mut crate::OperationContext::default(),
    )
}

/// Convert and verify a staged image using cumulative budgets and cancellation.
///
/// Each export pass and the full logical comparison are accounted separately.
/// Native exporters count source read calls; native file writes, metadata work
/// and syncs are backend work outside the context. Raw export counts reader and
/// writer calls. Native profiles require their fixed streaming-buffer ceiling;
/// raw export and comparison adapt their chunks to the caller's scratch limit.
/// Quotas are checked per pass; a later refusal retains earlier usage but removes
/// unpublished staging on a best-effort basis. Observers run after source reads,
/// during comparison and immediately before publication. They cannot interrupt
/// backend metadata work or revoke a published file. The source must be immutable.
pub fn convert_image_with_context(
    source: &dyn ReadAt,
    output: impl AsRef<Path>,
    format: ImageFormat,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<()> {
    materialize_with_context(source, output.as_ref(), format, false, context)
}

pub(crate) fn materialize_with_context(
    source: &dyn ReadAt,
    output: &Path,
    format: ImageFormat,
    sparse: bool,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<()> {
    let scratch = match format {
        ImageFormat::Raw => {
            if !source.is_empty() {
                context.require_scratch(2)?;
            }
            context.preflight(source.len(), 1, 2)?
        }
        ImageFormat::Qcow2 => 131072,
        ImageFormat::Vmdk => 67584,
        ImageFormat::Vdi | ImageFormat::Vhdx => 65536,
    };
    context.require_scratch(scratch)?;
    publish_image(output, |temporary| {
        context.scratch(scratch);
        match format {
            ImageFormat::Raw => {
                context.observe_phase(crate::OperationPhase::ImageExport, 0, source.len())?;
                let writer = RawWriter::create(temporary, source.len())?;
                let mut buffer = crate::operation_context::scratch_buffer(scratch)?;
                let mut offset = 0;
                while offset < source.len() {
                    let count = (source.len() - offset).min(buffer.len() as u64) as usize;
                    context.attempted_io();
                    source.read_exact_at(offset, &mut buffer[..count])?;
                    if !sparse || buffer[..count].iter().any(|&byte| byte != 0) {
                        context.attempted_io();
                        writer.write_all_at(offset, &buffer[..count])?;
                    }
                    offset += count as u64;
                    context.completed(count as u64);
                    context.observe_phase(
                        crate::OperationPhase::ImageExport,
                        offset,
                        source.len(),
                    )?;
                }
                writer.flush()?;
            }
            _ => {
                let mut input = crate::export_source::ContextSource::new(source, context);
                match format {
                    ImageFormat::Qcow2 => {
                        crate::qcow2_write::export_qcow2(temporary, &mut input, sparse).map(drop)?
                    }
                    ImageFormat::Vdi => crate::vdi_write::export_vdi(temporary, &mut input)?,
                    ImageFormat::Vhdx => crate::vhdx_write::export_vhdx(temporary, &mut input)?,
                    ImageFormat::Vmdk => {
                        crate::vmdk_write::export_vmdk(temporary, &mut input, false, None)
                            .map(drop)?
                    }
                    ImageFormat::Raw => unreachable!("raw export handled above"),
                }
            }
        }
        let reopened = Image::open(temporary, Some(format))?;
        if !compare_images_in_phase(
            source,
            &reopened,
            context,
            crate::OperationPhase::OutputVerification,
        )? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "materialized output differs from logical source",
            ));
        }
        if format == ImageFormat::Qcow2 {
            context.observe_phase(crate::OperationPhase::MetadataValidation, 0, 0)?;
            Qcow2::open(Arc::new(RawDisk::open(temporary)?))?.validate_active_mapping()?;
        }
        context.observe_phase(crate::OperationPhase::Publication, 0, 0)
    })
}

pub(crate) fn publish_image(
    output: &Path,
    prepare: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if fs::symlink_metadata(output).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output already exists",
        ));
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut entropy = [0; 16];
    getrandom::fill(&mut entropy).map_err(|e| io::Error::other(e.to_string()))?;
    let name: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
    let directory = parent.join(format!(".virtdisk-{name}"));
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&directory)?;
    let stage = Stage(directory);
    let temporary = stage.0.join("image");
    prepare(&temporary)?;
    fs::hard_link(&temporary, output)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("image"));
        let _ = fs::remove_dir(&self.0);
    }
}

/// Explicit policy for removing a logical disk tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShrinkPolicy {
    /// Reject every capacity reduction.
    Reject,
    /// Permit reduction only after verifying all removed logical bytes are zero.
    /// This does not prove partition or filesystem safety.
    RequireZero,
    /// Permit removing nonzero content; the caller accepts permanent data loss.
    AllowDataLoss,
}

/// Change capacity by materializing and publishing a new independent image.
///
/// Growth reads as zero. Shrink uses an explicit tail-removal policy and does
/// not update guest partition tables or filesystems. The original image and
/// its snapshot chain remain immutable; the output is flattened disk content.
pub fn resize_image(
    source: &dyn ReadAt,
    output: impl AsRef<Path>,
    format: ImageFormat,
    new_size: u64,
    policy: ShrinkPolicy,
) -> io::Result<()> {
    resize_image_with_context(
        source,
        output,
        format,
        new_size,
        policy,
        &mut crate::OperationContext::default(),
    )
}

/// Publish a resized independent image with shared budgets and cancellation.
///
/// `RequireZero` preflights and scans the removed tail in `TailValidation`, then
/// materialization and output verification use the same cumulative context.
/// Successful tail chunks, including a chunk that contains nonzero bytes, are
/// charged before reporting progress or refusal. Rejecting the shrink policy
/// performs no callbacks or I/O. Growth's synthetic zero ranges count as logical
/// work and facade read calls, not as physical source reads. Later export quota
/// or profile refusal retains prior scan usage. Before publication, failures
/// leave the output absent and staging cleanup is best effort. The source and
/// its authorized dependencies must remain immutable; guest filesystems are
/// not resized. See [`convert_image_with_context`] for publication guarantees.
pub fn resize_image_with_context(
    source: &dyn ReadAt,
    output: impl AsRef<Path>,
    format: ImageFormat,
    new_size: u64,
    policy: ShrinkPolicy,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<()> {
    let original_size = source.len();
    if new_size < original_size {
        match policy {
            ShrinkPolicy::Reject => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "shrink requires an explicit tail-removal policy",
                ));
            }
            ShrinkPolicy::RequireZero => {
                let tail_bytes = original_size - new_size;
                let scratch = context.preflight(tail_bytes, 1, 1)?;
                context.observe_phase(crate::OperationPhase::TailValidation, 0, tail_bytes)?;
                context.scratch(scratch);
                let mut buffer = crate::operation_context::scratch_buffer(scratch)?;
                let mut offset = new_size;
                while offset < original_size {
                    let count = (original_size - offset).min(buffer.len() as u64) as usize;
                    context.attempted_io();
                    source.read_exact_at(offset, &mut buffer[..count])?;
                    let nonzero = buffer[..count].iter().any(|b| *b != 0);
                    offset += count as u64;
                    context.completed(count as u64);
                    context.observe_phase(
                        crate::OperationPhase::TailValidation,
                        offset - new_size,
                        tail_bytes,
                    )?;
                    if nonzero {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "removed tail contains nonzero content",
                        ));
                    }
                }
            }
            ShrinkPolicy::AllowDataLoss => {}
        }
    }
    convert_image_with_context(
        &Resized {
            source,
            length: new_size,
        },
        output,
        format,
        context,
    )
}

struct Resized<'a> {
    source: &'a dyn ReadAt,
    length: u64,
}
impl ReadAt for Resized<'_> {
    fn len(&self) -> u64 {
        self.length
    }
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, destination.len() as u64, self.length)?;
        let count = self
            .source
            .len()
            .saturating_sub(offset)
            .min(destination.len() as u64) as usize;
        if count != 0 {
            self.source
                .read_exact_at(offset, &mut destination[..count])?;
        }
        destination[count..].fill(0);
        Ok(())
    }
    fn context(&self) -> ReadContext {
        self.source.context()
    }
    fn budget(&self) -> Option<ReadBudget> {
        self.source.budget()
    }
}
