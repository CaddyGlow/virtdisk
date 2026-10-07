//! Bounded format detection and logical image operations.
use crate::{
    ImageInspection, InspectImage, Qcow2, RawDisk, RawWriter, ReadAt, ReadBudget, ReadContext, Vdi,
    Vhdx, Vmdk, WriteAt,
};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// A virtual disk container family; support depends on its concrete profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// Unstructured logical disk bytes.
    Raw,
    /// QEMU copy-on-write version 2 container.
    Qcow2,
    /// Microsoft virtual hard disk version 2.
    Vhdx,
    /// VMware virtual machine disk.
    Vmdk,
    /// VirtualBox disk image.
    Vdi,
}

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
        let format = format.or(detect_format(&source)?).ok_or_else(|| {
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

/// Recognize a binary container signature with at most 68 bytes of input.
///
/// `None` means unrecognized, not validated raw data. Recognition does not
/// validate a container, and a recognized malformed image must not fall back
/// to raw. Text VMDK descriptors require explicit opening until supported.
pub fn detect_format(source: &dyn ReadAt) -> io::Result<Option<ImageFormat>> {
    let mut header = [0; 68];
    let length = source.len().min(header.len() as u64) as usize;
    source.read_exact_at(0, &mut header[..length])?;
    Ok(if length >= 4 && &header[..4] == b"QFI\xfb" {
        Some(ImageFormat::Qcow2)
    } else if length >= 8 && &header[..8] == b"vhdxfile" {
        Some(ImageFormat::Vhdx)
    } else if length >= 4 && &header[..4] == b"KDMV" {
        Some(ImageFormat::Vmdk)
    } else if length >= 68 && header[64..68] == 0xbeda107fu32.to_le_bytes() {
        Some(ImageFormat::Vdi)
    } else {
        None
    })
}

/// Compare logical lengths and bytes using bounded scratch memory.
///
/// Both inputs must remain immutable throughout the comparison. Allocation
/// layout and container metadata are not compared.
pub fn compare_images(left: &dyn ReadAt, right: &dyn ReadAt) -> io::Result<bool> {
    if left.len() != right.len() {
        return Ok(false);
    }
    let mut a = vec![0; 64 * 1024];
    let mut b = vec![0; a.len()];
    let mut offset = 0;
    while offset < left.len() {
        let count = (left.len() - offset).min(a.len() as u64) as usize;
        left.read_exact_at(offset, &mut a[..count])?;
        right.read_exact_at(offset, &mut b[..count])?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
        offset += count as u64;
    }
    Ok(true)
}

/// Hash immutable logical disk bytes with SHA-256 using bounded memory.
///
/// Container layout and metadata are not included. This is a content comparison
/// primitive, not evidence that the source was authenticated or remained stable.
pub fn hash_image(source: &dyn ReadAt) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut offset = 0;
    while offset < source.len() {
        let count = (source.len() - offset).min(buffer.len() as u64) as usize;
        source.read_exact_at(offset, &mut buffer[..count])?;
        hash.update(&buffer[..count]);
        offset += count as u64;
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

/// Copy logical bytes with cancellation checks before each bounded chunk.
///
/// Cancellation returns `Interrupted` and leaves already written chunks in the
/// output. No flush is implicit; use a disposable output for publication.
pub fn copy_image_with_cancel(
    source: &dyn ReadAt,
    output: &dyn WriteAt,
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<()> {
    if source.len() != output.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "image sizes differ",
        ));
    }
    let mut buffer = vec![0; 64 * 1024];
    let mut offset = 0;
    while offset < source.len() {
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "image copy cancelled",
            ));
        }
        let count = (source.len() - offset).min(buffer.len() as u64) as usize;
        source.read_exact_at(offset, &mut buffer[..count])?;
        output.write_all_at(offset, &buffer[..count])?;
        offset += count as u64;
    }
    Ok(())
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
    publish_image(output.as_ref(), |temporary| {
        match format {
            ImageFormat::Raw => {
                let writer = RawWriter::create(temporary, source.len())?;
                copy_image(source, &writer)?;
                writer.flush()?;
            }
            ImageFormat::Qcow2 => crate::create_qcow2(temporary, source)?,
            ImageFormat::Vdi => crate::create_vdi(temporary, source)?,
            ImageFormat::Vmdk => crate::create_vmdk(temporary, source)?,
            ImageFormat::Vhdx => crate::create_vhdx(temporary, source)?,
        }
        {
            let reopened = Image::open(temporary, Some(format))?;
            if !compare_images(source, &reopened)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "converted output differs from logical source",
                ));
            }
            if format == ImageFormat::Qcow2 {
                Qcow2::open(Arc::new(RawDisk::open(temporary)?))?.validate_active_mapping()?;
            }
        }
        Ok(())
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
    if new_size < source.len() {
        match policy {
            ShrinkPolicy::Reject => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "shrink requires an explicit tail-removal policy",
                ));
            }
            ShrinkPolicy::RequireZero => {
                let mut buffer = vec![0; 64 * 1024];
                let mut offset = new_size;
                while offset < source.len() {
                    let count = (source.len() - offset).min(buffer.len() as u64) as usize;
                    source.read_exact_at(offset, &mut buffer[..count])?;
                    if buffer[..count].iter().any(|b| *b != 0) {
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
    convert_image(
        &Resized {
            source,
            length: new_size,
        },
        output,
        format,
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
