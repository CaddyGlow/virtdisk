//! Stable CLI error records, separate from the library's typed I/O errors.
use super::json_output::JsonString;
use std::error::Error;
use virtdisk::io;
use virtdisk::{
    ImageFormat, OperationError, OperationLimitExceeded, OperationResource, ParserLimitExceeded,
    ParserResource,
};

#[derive(Clone, Copy)]
struct Quota {
    resource: &'static str,
    limit: u64,
    requested: u128,
}

pub(super) fn write_json(writer: &mut impl io::Write, error: &io::Error) -> io::Result<()> {
    let mut kind = kind_name(error.kind());
    let mut code = kind;
    let mut operation = None;
    let mut quota = None;
    let mut source: Option<&(dyn Error + 'static)> = Some(error);
    // Bound diagnostic traversal independently of parser depth.
    for _ in 0..64 {
        let Some(current) = source else { break };
        if let Some(value) = current.downcast_ref::<OperationError>() {
            operation.get_or_insert(value);
        }
        if let Some(value) = current.downcast_ref::<OperationLimitExceeded>() {
            code = "resource-limit";
            quota = Some(Quota {
                resource: match value.resource() {
                    OperationResource::LogicalBytes => "logical-bytes",
                    OperationResource::IoOperations => "io-operations",
                    OperationResource::ScratchBytes => "scratch-bytes",
                },
                limit: value.limit(),
                requested: value.requested(),
            });
        } else if let Some(value) = current.downcast_ref::<ParserLimitExceeded>() {
            code = "parser-limit";
            // Retain the established host CLI classification for parser bounds.
            kind = match value.resource() {
                ParserResource::RecursionDepth | ParserResource::AttributeBytes => "invalid-data",
                _ => "unsupported",
            };
            quota = Some(Quota {
                resource: match value.resource() {
                    ParserResource::MetadataBytes => "metadata-bytes",
                    ParserResource::CacheBytes => "cache-bytes",
                    ParserResource::WorkItems => "work-items",
                    ParserResource::DecompressedBytes => "decompressed-bytes",
                    ParserResource::DecompressionBufferBytes => "decompression-buffer-bytes",
                    ParserResource::RecursionDepth => "recursion-depth",
                    ParserResource::AttributeBytes => "attribute-bytes",
                    _ => "unknown",
                },
                limit: value.limit(),
                requested: value.requested(),
            });
        } else if current.is::<virtdisk::RecoveryRequired>() {
            code = "recovery-required";
        } else if current.is::<virtdisk::OperationCancelled>() {
            code = "cancelled";
        }
        source = if let Some(value) = current.downcast_ref::<io::Error>() {
            value
                .get_ref()
                .map(|value| value as &(dyn Error + 'static))
                .or_else(|| current.source())
        } else {
            current.source()
        };
    }
    let resource = quota.map(|value| value.resource);
    let range = operation.and_then(OperationError::range);
    Ok(writeln!(
        writer,
        "{{\"type\":\"error\",\"code\":\"{code}\",\"kind\":\"{kind}\",\"message\":{},\"operation\":{},\"format\":{},\"offset\":{},\"length\":{},\"resource\":{},\"limit\":{},\"requested\":{}}}",
        json_string(&error.to_string()),
        string_or_null(operation.map(|value| value.operation().as_str())),
        string_or_null(operation.map(|value| format_name(value.format()))),
        range.map_or_else(|| "null".to_owned(), |(offset, _)| offset.to_string()),
        range.map_or_else(|| "null".to_owned(), |(_, length)| length.to_string()),
        string_or_null(resource),
        quota.map_or_else(
            || "null".to_owned(),
            |value| json_string(&value.limit.to_string())
        ),
        quota.map_or_else(
            || "null".to_owned(),
            |value| json_string(&value.requested.to_string())
        ),
    )?)
}

fn string_or_null(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), json_string)
}

fn json_string(value: &str) -> String {
    JsonString(value).to_string()
}

fn kind_name(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "not-found",
        io::ErrorKind::PermissionDenied => "permission-denied",
        io::ErrorKind::AlreadyExists => "already-exists",
        io::ErrorKind::InvalidInput => "invalid-input",
        io::ErrorKind::InvalidData => "invalid-data",
        io::ErrorKind::UnexpectedEof => "unexpected-eof",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::WouldBlock => "would-block",
        io::ErrorKind::BrokenPipe => "broken-pipe",
        io::ErrorKind::WriteZero => "write-zero",
        io::ErrorKind::OutOfMemory => "out-of-memory",
        io::ErrorKind::TimedOut => "timed-out",
        io::ErrorKind::StorageFull => "storage-full",
        io::ErrorKind::QuotaExceeded => "quota-exceeded",
        io::ErrorKind::FileTooLarge => "file-too-large",
        io::ErrorKind::ReadOnlyFilesystem => "read-only-filesystem",
        io::ErrorKind::NotADirectory => "not-a-directory",
        io::ErrorKind::IsADirectory => "is-a-directory",
        io::ErrorKind::DirectoryNotEmpty => "directory-not-empty",
        io::ErrorKind::ResourceBusy => "resource-busy",
        io::ErrorKind::InvalidFilename => "invalid-filename",
        _ => "other",
    }
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Raw => "raw",
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Vhdx => "vhdx",
        ImageFormat::Vdi => "vdi",
        ImageFormat::Vmdk => "vmdk",
    }
}
