//! Dependency-independent storage errors retaining typed original causes.
use alloc::boxed::Box;
use core::{error::Error as CoreError, fmt};

/// Result returned by portable storage and parser operations.
pub type Result<T> = core::result::Result<T, Error>;

/// Machine-readable storage failure classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Requested bytes exceed the fixed source range.
    UnexpectedEof,
    /// Input metadata is corrupt.
    InvalidData,
    /// Caller arguments are invalid.
    InvalidInput,
    /// The format profile is unsupported.
    Unsupported,
    /// A configured resource ceiling was exceeded.
    ResourceLimit,
    /// The operation was cancelled or interrupted.
    Interrupted,
    /// Heap reservation failed.
    OutOfMemory,
    /// Reliable provider identity was not supplied.
    MissingIdentity,
    /// Backend storage was unavailable.
    NotFound,
    /// Backend access was denied.
    PermissionDenied,
    /// Backend resource must be retried later.
    WouldBlock,
    /// Backend resource already exists.
    AlreadyExists,
    /// Host backend classification: BrokenPipe.
    BrokenPipe,
    /// Host backend classification: WriteZero.
    WriteZero,
    /// Host backend classification: TimedOut.
    TimedOut,
    /// Host backend classification: StorageFull.
    StorageFull,
    /// Host backend classification: QuotaExceeded.
    QuotaExceeded,
    /// Host backend classification: FileTooLarge.
    FileTooLarge,
    /// Host backend classification: ReadOnlyFilesystem.
    ReadOnlyFilesystem,
    /// Host backend classification: NotADirectory.
    NotADirectory,
    /// Host backend classification: IsADirectory.
    IsADirectory,
    /// Host backend classification: DirectoryNotEmpty.
    DirectoryNotEmpty,
    /// Host backend classification: ResourceBusy.
    ResourceBusy,
    /// Host backend classification: InvalidFilename.
    InvalidFilename,
    /// Backend operation failed for another reason.
    Other,
}

/// Portable error with a retained, downcastable cause.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    source: Box<dyn CoreError + Send + Sync>,
}
impl Error {
    /// Retain a typed error or diagnostic string with its classification.
    pub fn new<E>(kind: ErrorKind, source: E) -> Self
    where
        E: Into<Box<dyn CoreError + Send + Sync>>,
    {
        Self {
            kind,
            source: source.into(),
        }
    }
    /// Retain a backend error with no more specific classification.
    pub fn other<E>(source: E) -> Self
    where
        E: Into<Box<dyn CoreError + Send + Sync>>,
    {
        Self::new(ErrorKind::Other, source)
    }
    /// Error classification independent of host features.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
    /// Typed cause available for downcasting.
    pub fn get_ref(&self) -> Option<&(dyn CoreError + Send + Sync + 'static)> {
        Some(self.source.as_ref())
    }
    /// Consume this error and retain the typed cause.
    pub fn into_inner(self) -> Option<Box<dyn CoreError + Send + Sync>> {
        Some(self.source)
    }
    /// Original host error, including through contextual error layers.
    #[cfg(feature = "std")]
    pub fn host_error(&self) -> Option<&std::io::Error> {
        let mut current: &(dyn CoreError + 'static) = self.source.as_ref();
        loop {
            if let Some(error) = current.downcast_ref::<std::io::Error>() {
                return Some(error);
            }
            current = current.source()?;
        }
    }
    /// Original OS error code, when a host error supplied one.
    #[cfg(feature = "std")]
    pub fn raw_os_error(&self) -> Option<i32> {
        self.host_error()?.raw_os_error()
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}
impl CoreError for Error {
    fn source(&self) -> Option<&(dyn CoreError + 'static)> {
        Some(self.source.as_ref())
    }
}
#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind as H;
        let kind = match error.kind() {
            H::UnexpectedEof => ErrorKind::UnexpectedEof,
            H::InvalidData => ErrorKind::InvalidData,
            H::InvalidInput => ErrorKind::InvalidInput,
            H::Unsupported => ErrorKind::Unsupported,
            H::Interrupted => ErrorKind::Interrupted,
            H::OutOfMemory => ErrorKind::OutOfMemory,
            H::NotFound => ErrorKind::NotFound,
            H::PermissionDenied => ErrorKind::PermissionDenied,
            H::WouldBlock => ErrorKind::WouldBlock,
            H::AlreadyExists => ErrorKind::AlreadyExists,
            H::BrokenPipe => ErrorKind::BrokenPipe,
            H::WriteZero => ErrorKind::WriteZero,
            H::TimedOut => ErrorKind::TimedOut,
            H::StorageFull => ErrorKind::StorageFull,
            H::QuotaExceeded => ErrorKind::QuotaExceeded,
            H::FileTooLarge => ErrorKind::FileTooLarge,
            H::ReadOnlyFilesystem => ErrorKind::ReadOnlyFilesystem,
            H::NotADirectory => ErrorKind::NotADirectory,
            H::IsADirectory => ErrorKind::IsADirectory,
            H::DirectoryNotEmpty => ErrorKind::DirectoryNotEmpty,
            H::ResourceBusy => ErrorKind::ResourceBusy,
            H::InvalidFilename => ErrorKind::InvalidFilename,
            _ => ErrorKind::Other,
        };
        Self::new(kind, error)
    }
}
#[cfg(feature = "std")]
impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        use std::io::ErrorKind as H;
        let kind = match error.kind {
            ErrorKind::UnexpectedEof => H::UnexpectedEof,
            ErrorKind::InvalidData => H::InvalidData,
            ErrorKind::InvalidInput => H::InvalidInput,
            ErrorKind::Unsupported | ErrorKind::ResourceLimit | ErrorKind::MissingIdentity => {
                H::Unsupported
            }
            ErrorKind::Interrupted => H::Interrupted,
            ErrorKind::OutOfMemory => H::OutOfMemory,
            ErrorKind::NotFound => H::NotFound,
            ErrorKind::PermissionDenied => H::PermissionDenied,
            ErrorKind::WouldBlock => H::WouldBlock,
            ErrorKind::AlreadyExists => H::AlreadyExists,
            ErrorKind::BrokenPipe => H::BrokenPipe,
            ErrorKind::WriteZero => H::WriteZero,
            ErrorKind::TimedOut => H::TimedOut,
            ErrorKind::StorageFull => H::StorageFull,
            ErrorKind::QuotaExceeded => H::QuotaExceeded,
            ErrorKind::FileTooLarge => H::FileTooLarge,
            ErrorKind::ReadOnlyFilesystem => H::ReadOnlyFilesystem,
            ErrorKind::NotADirectory => H::NotADirectory,
            ErrorKind::IsADirectory => H::IsADirectory,
            ErrorKind::DirectoryNotEmpty => H::DirectoryNotEmpty,
            ErrorKind::ResourceBusy => H::ResourceBusy,
            ErrorKind::InvalidFilename => H::InvalidFilename,
            ErrorKind::Other => H::Other,
        };
        Self::new(kind, error)
    }
}

/// Host stream adapters available only with the host feature.
#[cfg(feature = "std")]
pub use std::io::{BufRead, Cursor, Read, Seek, SeekFrom, Write, stderr, stdout};

#[cfg(feature = "std")]
impl PartialEq<std::io::ErrorKind> for ErrorKind {
    fn eq(&self, other: &std::io::ErrorKind) -> bool {
        let mapped = Error::from(std::io::Error::from(*other)).kind();
        *self == mapped
            || (*self == Self::ResourceLimit && *other == std::io::ErrorKind::Unsupported)
    }
}
#[cfg(feature = "std")]
impl PartialEq<ErrorKind> for std::io::ErrorKind {
    fn eq(&self, other: &ErrorKind) -> bool {
        other == self
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    #[test]
    fn host_error_retains_original_os_error_through_context() {
        let host = std::io::Error::from_raw_os_error(2);
        let portable = Error::from(host);
        assert_eq!(portable.kind(), ErrorKind::NotFound);
        assert_eq!(portable.raw_os_error(), Some(2));
        assert!(
            portable
                .get_ref()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .is_some()
        );
        let contextual = crate::ReadContext::default().error("read", portable);
        assert_eq!(contextual.raw_os_error(), Some(2));
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self::new(kind, alloc::format!("{kind:?}"))
    }
}
#[cfg(all(feature = "std", target_os = "linux"))]
impl From<rustix::io::Errno> for Error {
    fn from(error: rustix::io::Errno) -> Self {
        Self::from(std::io::Error::from(error))
    }
}
#[cfg(feature = "std")]
impl From<std::fs::TryLockError> for Error {
    fn from(error: std::fs::TryLockError) -> Self {
        Self::from(std::io::Error::from(error))
    }
}
