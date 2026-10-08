//! Portable format recognition.
use crate::{ReadAt, io};
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

/// Recognize a container header without validating its metadata.
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
