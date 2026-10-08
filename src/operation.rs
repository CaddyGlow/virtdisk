//! Structured provenance for common image operations.
use crate::{ImageFormat, ImageOperation};
use std::{fmt, io};

/// Operation provenance carried inside an `io::Error` without changing its kind.
///
/// This describes the attempted operation, not whether a failed mutation was
/// rolled back. Follow the concrete profile's recovery and failure contract.
#[derive(Debug)]
pub struct OperationError {
    operation: ImageOperation,
    format: ImageFormat,
    range: Option<(u64, u64)>,
    source: io::Error,
}
impl OperationError {
    /// Attempted operation.
    pub fn operation(&self) -> ImageOperation {
        self.operation
    }
    /// Explicitly selected container family.
    pub fn format(&self) -> ImageFormat {
        self.format
    }
    /// Attempted logical byte offset and length, when the operation uses a range.
    pub fn range(&self) -> Option<(u64, u64)> {
        self.range
    }
    pub(crate) fn wrap(
        operation: ImageOperation,
        format: ImageFormat,
        range: Option<(u64, u64)>,
        source: io::Error,
    ) -> io::Error {
        io::Error::new(
            source.kind(),
            Self {
                operation,
                format,
                range,
                source,
            },
        )
    }
}
impl fmt::Display for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} {:?}", self.format, self.operation)?;
        if let Some((offset, length)) = self.range {
            write!(f, " offset={offset} length={length}")?;
        }
        write!(f, ": {}", self.source)
    }
}
impl std::error::Error for OperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
