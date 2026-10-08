//! Bounded caller-owned dependency declarations, independent of image authority.
use crate::io::{self, Read, Write};
use crate::{ImageFormat, ImageGraph, ImageSpec, ParserLimits, ReadBudget};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[path = "graph_manifest_replace.rs"]
pub(crate) mod replacement;

const MAGIC: &[u8; 8] = b"VDGRAPH\0";
const MAX_BYTES: usize = 1_048_576;
const MAX_PATH: usize = 65_536;
const NONE: u16 = u16::MAX;

/// A versioned external snapshot graph declaration and optional selected state.
///
/// Paths and edges do not grant file access or establish immutable content.
/// Opening the live graph requires an exact caller-authorized path set and
/// revalidates native parent metadata. Manifests cannot discover unknown children.
/// Version 1 stores absolute paths, up to 128 images and 1 MiB, with a corruption
/// checksum. Unicode paths use UTF-8; non-Unicode paths use native encoding.
#[derive(Clone, Debug)]
pub struct GraphManifest {
    images: Vec<ImageSpec>,
    selected: Option<PathBuf>,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn copied(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(bytes.len())
        .map_err(io::Error::other)?;
    result.extend_from_slice(bytes);
    Ok(result)
}
fn read_bytes(file: &mut File) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || !(48..=MAX_BYTES as u64).contains(&metadata.len()) {
        return Err(invalid(
            "manifest must be a regular file between 48 bytes and 1 MiB",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(metadata.len() as usize)
        .map_err(io::Error::other)?;
    bytes.resize(metadata.len() as usize, 0);
    file.read_exact(&mut bytes)?;
    if file.read(&mut [0])? != 0 {
        return Err(invalid("manifest changed length while reading"));
    }
    Ok(bytes)
}
impl GraphManifest {
    pub(crate) fn new(images: Vec<ImageSpec>, selected: Option<PathBuf>) -> Self {
        Self { images, selected }
    }
    /// Registered images in manifest order; parent paths refer to this same set.
    pub fn images(&self) -> &[ImageSpec] {
        &self.images
    }
    /// Selected disk state, or no selection. Selection does not mutate any image.
    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }
    /// Parse a bounded regular manifest file without opening any image paths.
    /// On Linux the open is nonblocking, so FIFO paths reach the regular-file
    /// check without waiting for a producer. The checksum detects corruption,
    /// not malicious modification or stale data.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        let mut file = File::from(rustix::fs::open(
            path.as_ref(),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )?);
        #[cfg(not(target_os = "linux"))]
        let mut file = File::open(path)?;
        let bytes = read_bytes(&mut file)?;
        Self::decode(&bytes)
    }
    /// Publish a new manifest without overwriting an existing destination.
    /// The file is synced before publication. Directory-sync failure can occur
    /// after publication. Image files are not modified or atomically committed
    /// together with the manifest; callers serialize their graph updates.
    pub fn save(&self, output: impl AsRef<Path>) -> io::Result<()> {
        let bytes = self.encode()?;
        crate::image::publish_image(output.as_ref(), |temporary| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(temporary)?;
            file.write_all(&bytes)?;
            Ok(file.sync_all()?)
        })
    }
    /// Bind this declaration to exactly the paths authorized by the caller.
    ///
    /// Only supplied paths are canonicalized before authority comparison; a
    /// manifest cannot induce opening another image. Missing, extra or duplicate
    /// grants are refused. Normal graph validation checks aliases, native edges,
    /// cycles and depth. This freshly binds live file identities, rather than
    /// proving image bytes or identities have remained unchanged since saving.
    /// Callers still declare complete dependency ownership for destructive work.
    pub fn open_graph(&self, authorized_paths: &[PathBuf]) -> io::Result<ImageGraph> {
        self.open_graph_inner(authorized_paths, None)
    }
    /// Bind exact path authority using one cumulative graph parser budget.
    /// Limits are validated before authority path access. Authorization,
    /// registration, graph readers and deferred reads share accounting;
    /// manifest file parsing retains its separate fixed framing bounds.
    pub fn open_graph_with_limits(
        &self,
        authorized_paths: &[PathBuf],
        limits: ParserLimits,
    ) -> io::Result<ImageGraph> {
        self.open_graph_inner(authorized_paths, Some(ReadBudget::new(limits)?))
    }
    fn open_graph_inner(
        &self,
        authorized_paths: &[PathBuf],
        budget: Option<ReadBudget>,
    ) -> io::Result<ImageGraph> {
        if authorized_paths.len() != self.images.len() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "manifest authority must cover exactly its images",
            ));
        }
        let mut authorized = Vec::new();
        if let Some(budget) = &budget {
            budget.metadata(std::mem::size_of_val(authorized_paths) as u64)?;
        }
        authorized
            .try_reserve_exact(authorized_paths.len())
            .map_err(io::Error::other)?;
        for path in authorized_paths {
            if let Some(budget) = &budget {
                budget.work(1)?;
                budget.metadata(path.as_os_str().as_encoded_bytes().len() as u64 * 2)?;
            }
            let path = path.canonicalize()?;
            if let Some(budget) = &budget {
                budget.metadata(path.as_os_str().as_encoded_bytes().len() as u64)?;
            }
            if authorized.contains(&path) {
                return Err(invalid("duplicate manifest authority"));
            }
            authorized.push(path);
        }
        if self
            .images
            .iter()
            .any(|image| !authorized.contains(&image.path))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "manifest contains an unauthorized image",
            ));
        }
        ImageGraph::open_inner(&self.images, budget)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn published_leaf(mut self, staged: &Path, published: &Path) -> io::Result<Self> {
        if !published.is_absolute()
            || self
                .images
                .iter()
                .any(|image| image.path == published || image.parent.as_deref() == Some(staged))
        {
            return Err(invalid("generation leaf publication path is invalid"));
        }
        let image = self
            .images
            .iter_mut()
            .find(|image| image.path == staged)
            .ok_or_else(|| invalid("generation leaf is not registered"))?;
        image.path = published.to_path_buf();
        if self.selected.as_deref() == Some(staged) {
            self.selected = Some(published.to_path_buf());
        }
        Ok(self)
    }
    fn encode(&self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        append(&mut bytes, MAGIC)?;
        append(&mut bytes, &1u32.to_le_bytes())?;
        append(&mut bytes, &(self.images.len() as u16).to_le_bytes())?;
        let index = |path: &Path| -> io::Result<u16> {
            self.images
                .iter()
                .position(|image| image.path == path)
                .map(|i| i as u16)
                .ok_or_else(|| invalid("manifest path is not registered"))
        };
        let selected = self
            .selected
            .as_deref()
            .map(index)
            .transpose()?
            .unwrap_or(NONE);
        append(&mut bytes, &selected.to_le_bytes())?;
        for image in &self.images {
            let format = match image.format {
                ImageFormat::Raw => 0,
                ImageFormat::Qcow2 => 1,
                ImageFormat::Vhdx => 2,
                ImageFormat::Vdi => 3,
                ImageFormat::Vmdk => 4,
            };
            let (codec, path) = encode_path(&image.path)?;
            append(&mut bytes, &[format, codec])?;
            append(
                &mut bytes,
                &image
                    .parent
                    .as_deref()
                    .map(index)
                    .transpose()?
                    .unwrap_or(NONE)
                    .to_le_bytes(),
            )?;
            append(&mut bytes, &(path.len() as u32).to_le_bytes())?;
            append(&mut bytes, &path)?;
        }
        let checksum = Sha256::digest(&bytes);
        bytes.try_reserve_exact(32).map_err(io::Error::other)?;
        bytes.extend_from_slice(&checksum);
        Ok(bytes)
    }
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let payload = bytes
            .len()
            .checked_sub(32)
            .ok_or_else(|| invalid("truncated manifest"))?;
        if Sha256::digest(&bytes[..payload]).as_slice() != &bytes[payload..] {
            return Err(invalid("manifest checksum mismatch"));
        }
        let mut cursor = Cursor(&bytes[..payload]);
        if cursor.take(8)? != MAGIC {
            return Err(invalid("invalid manifest signature"));
        }
        if cursor.u32()? != 1 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported graph manifest version",
            ));
        }
        let count = cursor.u16()? as usize;
        let selected = cursor.u16()?;
        if count > 128 || (selected != NONE && selected as usize >= count) {
            return Err(invalid("manifest node count or selection is invalid"));
        }
        let mut images: Vec<ImageSpec> = Vec::new();
        images.try_reserve_exact(count).map_err(io::Error::other)?;
        let mut parents = Vec::new();
        parents.try_reserve_exact(count).map_err(io::Error::other)?;
        for _ in 0..count {
            let tags = cursor.take(2)?;
            let format = match tags[0] {
                0 => ImageFormat::Raw,
                1 => ImageFormat::Qcow2,
                2 => ImageFormat::Vhdx,
                3 => ImageFormat::Vdi,
                4 => ImageFormat::Vmdk,
                _ => return Err(invalid("unknown manifest image format")),
            };
            let parent = cursor.u16()?;
            if parent != NONE && parent as usize >= count {
                return Err(invalid("manifest parent index is invalid"));
            }
            let length = cursor.u32()? as usize;
            if length == 0 || length > MAX_PATH {
                return Err(invalid("manifest path exceeds bounds"));
            }
            let path = decode_path(tags[1], cursor.take(length)?)?;
            if !path.is_absolute() || images.iter().any(|image| image.path == path) {
                return Err(invalid("manifest paths must be absolute and unique"));
            }
            images.push(ImageSpec {
                path,
                format,
                parent: None,
            });
            parents.push(parent);
        }
        if !cursor.0.is_empty() {
            return Err(invalid("trailing manifest payload"));
        }
        for (i, &parent) in parents.iter().enumerate() {
            if parent != NONE {
                let parent = parent as usize;
                let valid_family = match images[i].format {
                    ImageFormat::Raw => false,
                    ImageFormat::Qcow2 => {
                        matches!(images[parent].format, ImageFormat::Raw | ImageFormat::Qcow2)
                    }
                    family => images[parent].format == family,
                };
                if !valid_family {
                    return Err(invalid("manifest parent family is invalid"));
                }
                images[i].parent = Some(images[parent].path.clone());
            }
            let mut current = i;
            let mut visited = Vec::new();
            while parents[current] != NONE {
                if visited.contains(&current) || visited.len() >= 31 {
                    return Err(invalid("manifest graph is cyclic or too deep"));
                }
                visited.push(current);
                current = parents[current] as usize;
            }
        }
        let selected = (selected != NONE).then(|| images[selected as usize].path.clone());
        Ok(Self { images, selected })
    }
}
fn append(output: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    if output.len().saturating_add(bytes.len()) > MAX_BYTES - 32 {
        return Err(invalid("manifest exceeds 1 MiB"));
    }
    output
        .try_reserve_exact(bytes.len())
        .map_err(io::Error::other)?;
    output.extend_from_slice(bytes);
    Ok(())
}
struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        let (prefix, rest) = self
            .0
            .split_at_checked(length)
            .ok_or_else(|| invalid("truncated manifest payload"))?;
        self.0 = rest;
        Ok(prefix)
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}
fn encode_path(path: &Path) -> io::Result<(u8, Vec<u8>)> {
    if let Some(path) = path.to_str() {
        if path.len() > MAX_PATH {
            return Err(invalid("manifest path exceeds bounds"));
        }
        return Ok((0, copied(path.as_bytes())?));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        if bytes.len() > MAX_PATH {
            return Err(invalid("manifest path exceeds bounds"));
        }
        Ok((1, copied(bytes)?))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let length = path
            .as_os_str()
            .encode_wide()
            .count()
            .checked_mul(2)
            .ok_or_else(|| invalid("manifest path exceeds bounds"))?;
        if length > MAX_PATH {
            return Err(invalid("manifest path exceeds bounds"));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(io::Error::other)?;
        for word in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        Ok((2, bytes))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "non-Unicode manifest paths are unsupported on this platform",
        ))
    }
}
fn decode_path(codec: u8, bytes: &[u8]) -> io::Result<PathBuf> {
    if codec == 0 {
        let text = String::from_utf8(copied(bytes)?)
            .map_err(|_| invalid("invalid Unicode manifest path"))?;
        if text.contains('\0') {
            return Err(invalid("manifest path contains NUL"));
        }
        return Ok(PathBuf::from(text));
    }
    #[cfg(unix)]
    if codec == 1 {
        use std::os::unix::ffi::OsStringExt;
        if bytes.contains(&0) {
            return Err(invalid("manifest path contains NUL"));
        }
        return Ok(PathBuf::from(std::ffi::OsString::from_vec(copied(bytes)?)));
    }
    #[cfg(windows)]
    if codec == 2 {
        use std::os::windows::ffi::OsStringExt;
        if !bytes.len().is_multiple_of(2) {
            return Err(invalid("invalid Windows manifest path"));
        }
        let mut words = Vec::new();
        words
            .try_reserve_exact(bytes.len() / 2)
            .map_err(io::Error::other)?;
        for pair in bytes.as_chunks::<2>().0 {
            words.push(u16::from_le_bytes([pair[0], pair[1]]));
        }
        if words.contains(&0) {
            return Err(invalid("manifest path contains NUL"));
        }
        return Ok(PathBuf::from(std::ffi::OsString::from_wide(&words)));
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "manifest path encoding is unsupported on this platform",
    ))
}

#[cfg(all(test, windows))]
mod tests {
    use super::{decode_path, encode_path};
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

    #[test]
    fn native_windows_encoding_retains_unpaired_surrogates() {
        let path = PathBuf::from(OsString::from_wide(&[b'C' as u16, 58, 92, 0xd800]));
        let (codec, encoded) = encode_path(&path).unwrap();
        assert_eq!(codec, 2);
        assert_eq!(decode_path(codec, &encoded).unwrap(), path);
        assert!(decode_path(2, &[1]).is_err());
        assert!(decode_path(2, &[0, 0]).is_err());
    }
}
