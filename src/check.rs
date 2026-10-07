//! Read-only checks with explicit structural and logical payload scope.
use crate::{Image, ImageFormat, Qcow2, ReadAt};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Optional work beyond supported container ownership validation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckOptions {
    /// Read every current logical byte and audit QCOW2 compressed descriptors
    /// in current and saved disk states. This cannot authenticate payload bytes.
    pub payload: bool,
}

/// Structural evidence established by a successful check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckScope {
    /// Raw has no container ownership metadata; only its logical length is known.
    RawLengthOnly,
    /// The supported profile's parsed metadata and recognized allocation owners
    /// passed validation. Unknown required features cause an error. Optional
    /// opaque extension ranges may be checked without interpreting their
    /// contents, so this does not establish every extension's internal validity.
    /// Guest filesystems and external runtime compatibility are outside this scope.
    SupportedContainerOwnership,
}

/// A successful bounded check; an error never returns a partial success report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckReport {
    /// Explicitly selected container family.
    pub format: ImageFormat,
    /// Current addressable logical byte count.
    pub virtual_size: u64,
    /// Structural scope established by the format validator.
    pub structural: CheckScope,
    /// Current logical bytes actually swept; zero when no sweep was requested.
    /// This excludes QCOW2 saved-state compressed descriptor audit reads.
    pub payload_bytes_read: u64,
}

/// Check an immutable image using only explicitly authorized reference paths.
///
/// All input files must remain immutable throughout checking. This operation
/// does not lock them against other writers or repair any metadata. Authorized
/// paths follow the same ordering and reference rules as [`Image::open_chain`].
pub fn check_image(
    path: impl AsRef<Path>,
    format: ImageFormat,
    authorized_paths: &[PathBuf],
    options: CheckOptions,
) -> io::Result<CheckReport> {
    check_image_with_cancel(path, format, authorized_paths, options, || false)
}

/// Check with cancellation before opening, during QCOW2 ownership traversal,
/// and before every bounded payload chunk.
///
/// Other families' existing bounded open-time metadata parsing is not
/// interruptible within a constructor. Cancellation returns `Interrupted`;
/// all successful reads still require an externally immutable input.
pub fn check_image_with_cancel(
    path: impl AsRef<Path>,
    format: ImageFormat,
    authorized_paths: &[PathBuf],
    options: CheckOptions,
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<CheckReport> {
    cancel(&mut cancelled)?;
    let path = path.as_ref();
    let reader: Box<dyn ReadAt> = if format == ImageFormat::Qcow2 {
        let image = Qcow2::open_chain(path, authorized_paths)?;
        if options.payload {
            image.validate_active_mapping_and_compressed_payloads_with_cancel(&mut cancelled)?;
        } else {
            image.validate_active_mapping_with_cancel(&mut cancelled)?;
        }
        Box::new(image)
    } else {
        Box::new(Image::open_chain(path, Some(format), authorized_paths)?)
    };
    cancel(&mut cancelled)?;
    let payload_bytes_read = if options.payload {
        check_payload_with_cancel(reader.as_ref(), &mut cancelled)?
    } else {
        0
    };
    Ok(CheckReport {
        format,
        virtual_size: reader.len(),
        structural: if format == ImageFormat::Raw {
            CheckScope::RawLengthOnly
        } else {
            CheckScope::SupportedContainerOwnership
        },
        payload_bytes_read,
    })
}

/// Sweep the current logical view using a 64 KiB scratch buffer.
///
/// Returns the number of bytes read on complete success. Preserves source read
/// errors and their provenance. Reading is evidence of accessibility, not data
/// authenticity; unused physical bytes and saved logical views are not swept.
pub fn check_payload_with_cancel(
    source: &dyn ReadAt,
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<u64> {
    let mut buffer = vec![0; 64 * 1024];
    let mut offset = 0;
    while offset < source.len() {
        cancel(&mut cancelled)?;
        let length = (source.len() - offset).min(buffer.len() as u64) as usize;
        source.read_exact_at(offset, &mut buffer[..length])?;
        offset += length as u64;
    }
    Ok(offset)
}

fn cancel(cancelled: &mut impl FnMut() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "image check cancelled",
        ))
    } else {
        Ok(())
    }
}
