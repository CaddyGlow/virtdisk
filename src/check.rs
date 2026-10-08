//! Read-only checks with explicit structural and logical payload scope.
use crate::io;
use crate::{Image, ImageFormat, Qcow2, ReadAt};
use std::path::{Path, PathBuf};

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
/// Validate supported ownership and optionally sweep payload using a caller context.
///
/// Metadata progress uses `MetadataValidation` with zero logical byte counters.
/// Open-time parser budgets apply separately; context byte/I/O/scratch limits
/// cover the logical payload sweep, not metadata or compressed-descriptor audits.
/// Cancellation is checked before opening, during QCOW2 validation and after
/// constructors. Other formats cannot interrupt their bounded constructors.
/// All inputs must remain immutable; this never repairs or modifies image bytes.
/// No success report is returned on cancellation or error.
pub fn check_image_with_context(
    path: impl AsRef<Path>,
    format: ImageFormat,
    authorized_paths: &[PathBuf],
    options: CheckOptions,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<CheckReport> {
    check_image_with_limits_and_context(
        path,
        format,
        authorized_paths,
        options,
        crate::ParserLimits::default(),
        context,
    )
}

/// Check with caller-tightened parser budgets and a separate payload context.
///
/// Limits are validated before progress callbacks or file access. One parser
/// budget covers opening, authorized dependencies, QCOW2 ownership validation
/// and deferred payload reads. The context retains its independent logical
/// payload accounting and cancellation contract. No recovery is performed.
pub fn check_image_with_limits_and_context(
    path: impl AsRef<Path>,
    format: ImageFormat,
    authorized_paths: &[PathBuf],
    options: CheckOptions,
    limits: crate::ParserLimits,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<CheckReport> {
    limits.validate()?;
    use crate::OperationPhase::MetadataValidation;
    context.observe_phase(MetadataValidation, 0, 0)?;
    let mut interruption = None;
    let opened = {
        let mut cancelled = || {
            if let Err(error) = context.observe_phase(MetadataValidation, 0, 0) {
                interruption = Some(error);
                true
            } else {
                false
            }
        };
        open_checked(
            path.as_ref(),
            format,
            authorized_paths,
            options,
            limits,
            &mut cancelled,
        )
    };
    let reader = opened.map_err(|error| interruption.unwrap_or(error))?;
    context.observe_phase(MetadataValidation, 0, 0)?;
    let payload_bytes_read = if options.payload {
        check_payload_with_context(reader.as_ref(), context)?
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

/// Check with validated caller parser ceilings and default payload context.
/// All sources remain immutable; this never repairs or recovers metadata.
pub fn check_image_with_limits(
    path: impl AsRef<Path>,
    format: ImageFormat,
    authorized_paths: &[PathBuf],
    options: CheckOptions,
    limits: crate::ParserLimits,
) -> io::Result<CheckReport> {
    check_image_with_limits_and_context(
        path,
        format,
        authorized_paths,
        options,
        limits,
        &mut crate::OperationContext::default(),
    )
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
    let reader = open_checked(
        path,
        format,
        authorized_paths,
        options,
        crate::ParserLimits::default(),
        &mut cancelled,
    )?;
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
    let mut observer = |progress: crate::OperationProgress| {
        if progress.completed_bytes < progress.total_bytes && cancelled() {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    let mut context = crate::OperationContext::default().with_observer(&mut observer);
    check_payload_with_context(source, &mut context)
}
/// Sweep current logical bytes with cumulative caller budgets and phase progress.
///
/// The complete request is preflighted before callbacks, allocation or payload
/// I/O. Scratch is fallible and at most 64 KiB. Progress uses `PayloadValidation`
/// before reads and after each complete chunk; cancellation at any boundary
/// returns `Interrupted`, including after the last chunk. Read provenance and
/// failed I/O attempts are preserved. Reading proves accessibility, not content
/// authenticity; saved views and unused physical bytes are not swept.
pub fn check_payload_with_context(
    source: &dyn ReadAt,
    context: &mut crate::OperationContext<'_>,
) -> io::Result<u64> {
    use crate::OperationPhase::PayloadValidation;
    let size = source.len();
    let scratch = context.preflight(size, 1, 1)?;
    context.observe_phase(PayloadValidation, 0, size)?;
    context.scratch(scratch);
    let mut buffer = crate::operation_context::scratch_buffer(scratch)?;
    let mut offset = 0;
    while offset < size {
        let count = (size - offset).min(scratch as u64) as usize;
        context.attempted_io();
        source.read_exact_at(offset, &mut buffer[..count])?;
        offset += count as u64;
        context.completed(count as u64);
        context.observe_phase(PayloadValidation, offset, size)?;
    }
    Ok(size)
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

fn open_checked(
    path: &Path,
    format: ImageFormat,
    authorized_paths: &[PathBuf],
    options: CheckOptions,
    limits: crate::ParserLimits,
    cancelled: &mut dyn FnMut() -> bool,
) -> io::Result<Box<dyn ReadAt>> {
    let reader: Box<dyn ReadAt> = if format == ImageFormat::Qcow2 {
        let image = Qcow2::open_chain_with_limits(path, authorized_paths, limits)?;
        if options.payload {
            image.validate_active_mapping_and_compressed_payloads_with_cancel(&mut *cancelled)?;
        } else {
            image.validate_active_mapping_with_cancel(&mut *cancelled)?;
        }
        Box::new(image)
    } else {
        let opening = crate::ReaderOpenOptions::default()
            .format(format)
            .authorized_paths(authorized_paths.iter().cloned())
            .parser_limits(limits)?;
        Box::new(Image::open_with_options(path, &opening)?)
    };
    Ok(reader)
}
