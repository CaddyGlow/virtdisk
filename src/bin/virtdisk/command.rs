//! Command syntax is parsed before execution; paths remain borrowed OS strings.
use std::ffi::{OsStr, OsString};
use virtdisk::{DiscardPolicy, ImageFormat, ShrinkPolicy, io};

pub(super) enum Access {
    Read,
    Write,
}
impl Access {
    fn parse(value: &OsStr) -> io::Result<Self> {
        match value.to_str() {
            Some("read") => Ok(Self::Read),
            Some("write") => Ok(Self::Write),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "capabilities access must be read or write",
            )),
        }
    }
}

pub(super) enum Command<'a> {
    GraphSnapshot {
        manifest: &'a OsStr,
        parent: &'a OsStr,
        directory: &'a OsStr,
        family: ImageFormat,
        authorized: &'a [OsString],
    },
    GraphFlatten {
        manifest: &'a OsStr,
        source: &'a OsStr,
        output: &'a OsStr,
        family: ImageFormat,
        authorized: &'a [OsString],
    },
    GraphMerge {
        manifest: &'a OsStr,
        source: &'a OsStr,
        ancestor: &'a OsStr,
        output: &'a OsStr,
        family: ImageFormat,
        authorized: &'a [OsString],
    },
    GraphRebase {
        manifest: &'a OsStr,
        source: &'a OsStr,
        parent: &'a OsStr,
        output: &'a OsStr,
        authorized: &'a [OsString],
        generation: bool,
    },
    GraphSelectInPlace {
        path: &'a OsStr,
        selected: &'a OsStr,
        authorized: &'a [OsString],
    },
    GraphSelect {
        manifest: &'a OsStr,
        selected: &'a OsStr,
        output: &'a OsStr,
        authorized: &'a [OsString],
    },
    GraphInfo {
        manifest: &'a OsStr,
        authorized: &'a [OsString],
    },
    SnapshotChange {
        path: &'a OsStr,
        id: Vec<u8>,
        parents: &'a [OsString],
        delete: bool,
    },
    SnapshotCreate {
        path: &'a OsStr,
        id: Vec<u8>,
        name: Vec<u8>,
        parents: &'a [OsString],
    },
    Check {
        path: &'a OsStr,
        selected: ImageFormat,
        options: virtdisk::CheckOptions,
        parents: &'a [OsString],
    },
    SnapshotList {
        path: &'a OsStr,
        parents: &'a [OsString],
    },
    SnapshotExport {
        path: &'a OsStr,
        id: Vec<u8>,
        output: &'a OsStr,
        selected: ImageFormat,
        parents: &'a [OsString],
    },
    ResizeNative {
        path: &'a OsStr,
        selected: ImageFormat,
        size: u64,
        policy: ShrinkPolicy,
        parents: &'a [OsString],
    },
    Preallocate {
        path: &'a OsStr,
        selected: ImageFormat,
        offset: u64,
        length: u64,
    },
    Create {
        path: &'a OsStr,
        selected: ImageFormat,
        size: u64,
        sparse: bool,
    },
    Trim {
        path: &'a OsStr,
        selected: ImageFormat,
        offset: u64,
        length: u64,
        policy: DiscardPolicy,
        parents: &'a [OsString],
    },
    Zero {
        path: &'a OsStr,
        selected: ImageFormat,
        offset: u64,
        length: u64,
        parents: &'a [OsString],
    },
    Hash {
        path: &'a OsStr,
        selected: ImageFormat,
        parents: &'a [OsString],
    },
    Map {
        path: &'a OsStr,
        selected: ImageFormat,
        parents: &'a [OsString],
    },
    Resize {
        input: &'a OsStr,
        from: ImageFormat,
        output: &'a OsStr,
        to: ImageFormat,
        size: u64,
        policy: ShrinkPolicy,
    },
    Capabilities {
        access: Access,
        path: &'a OsStr,
        selected: ImageFormat,
        parents: &'a [OsString],
    },
    Info {
        path: &'a OsStr,
        selected: ImageFormat,
        parents: &'a [OsString],
    },
    Convert {
        input: &'a OsStr,
        from: ImageFormat,
        output: &'a OsStr,
        to: ImageFormat,
    },
    Compact {
        input: &'a OsStr,
        from: ImageFormat,
        output: &'a OsStr,
        to: ImageFormat,
    },
    Compare {
        left: &'a OsStr,
        left_format: ImageFormat,
        right: &'a OsStr,
        right_format: ImageFormat,
    },
}

impl<'a> Command<'a> {
    pub(super) fn parse(args: &'a [OsString]) -> io::Result<Self> {
        match args {
            [
                command,
                action,
                manifest,
                parent,
                directory,
                family,
                authorized @ ..,
            ] if command == "graph" && action == "snapshot" => Ok(Command::GraphSnapshot {
                manifest,
                parent,
                directory,
                family: format(family)?,
                authorized,
            }),
            [
                command,
                action,
                manifest,
                source,
                output,
                family,
                authorized @ ..,
            ] if command == "graph" && action == "flatten" => Ok(Command::GraphFlatten {
                manifest,
                source,
                output,
                family: format(family)?,
                authorized,
            }),
            [
                command,
                action,
                manifest,
                source,
                ancestor,
                output,
                family,
                authorized @ ..,
            ] if command == "graph" && action == "merge" => Ok(Command::GraphMerge {
                manifest,
                source,
                ancestor,
                output,
                family: format(family)?,
                authorized,
            }),
            [
                command,
                action,
                manifest,
                source,
                parent,
                output,
                authorized @ ..,
            ] if command == "graph" && (action == "rebase" || action == "rebase-generation") => {
                Ok(Command::GraphRebase {
                    manifest,
                    source,
                    parent,
                    output,
                    authorized,
                    generation: action == "rebase-generation",
                })
            }
            [command, action, path, selected, authorized @ ..]
                if command == "graph" && action == "select-in-place" =>
            {
                Ok(Command::GraphSelectInPlace {
                    path,
                    selected,
                    authorized,
                })
            }
            [command, action, manifest, selected, output, authorized @ ..]
                if command == "graph" && action == "select" =>
            {
                Ok(Command::GraphSelect {
                    manifest,
                    selected,
                    output,
                    authorized,
                })
            }
            [command, action, manifest, authorized @ ..]
                if command == "graph" && action == "info" =>
            {
                Ok(Command::GraphInfo {
                    manifest,
                    authorized,
                })
            }
            [command, action, path, id, parents @ ..]
                if command == "snapshot" && (action == "delete" || action == "revert") =>
            {
                Ok(Command::SnapshotChange {
                    path,
                    id: snapshot_bytes(id)?,
                    parents,
                    delete: action == "delete",
                })
            }
            [command, action, path, id, name, parents @ ..]
                if command == "snapshot" && action == "create" =>
            {
                Ok(Command::SnapshotCreate {
                    path,
                    id: snapshot_bytes(id)?,
                    name: snapshot_bytes(name)?,
                    parents,
                })
            }
            [command, path, selected, mode, parents @ ..] if command == "check" => {
                let options = virtdisk::CheckOptions {
                    payload: match mode.to_str() {
                        Some("structure") => false,
                        Some("payload") => true,
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "check mode must be structure or payload",
                            ));
                        }
                    },
                };
                Ok(Command::Check {
                    path,
                    selected: format(selected)?,
                    options,
                    parents,
                })
            }
            [command, action, path, parents @ ..] if command == "snapshot" && action == "list" => {
                Ok(Command::SnapshotList { path, parents })
            }
            [command, action, path, id, output, selected, parents @ ..]
                if command == "snapshot" && action == "export" =>
            {
                Ok(Command::SnapshotExport {
                    path,
                    id: snapshot_bytes(id)?,
                    output,
                    selected: format(selected)?,
                    parents,
                })
            }
            [command, path, selected, size, policy, parents @ ..] if command == "resize-native" => {
                Ok(Command::ResizeNative {
                    path,
                    selected: format(selected)?,
                    size: byte_count(size)?,
                    policy: shrink_policy(policy)?,
                    parents,
                })
            }
            [command, path, selected, offset, length] if command == "preallocate" => {
                Ok(Command::Preallocate {
                    path,
                    selected: format(selected)?,
                    offset: byte_count(offset)?,
                    length: byte_count(length)?,
                })
            }
            [command, path, selected, size]
                if command == "create" || command == "create-sparse" =>
            {
                Ok(Command::Create {
                    path,
                    selected: format(selected)?,
                    size: byte_count(size)?,
                    sparse: command == "create-sparse",
                })
            }
            [
                command,
                path,
                selected,
                offset,
                length,
                policy,
                parents @ ..,
            ] if command == "trim" => {
                let policy = match policy.to_str() {
                    Some("require") => DiscardPolicy::RequireDeallocation,
                    Some("zero-fallback") => DiscardPolicy::AllowZeroFallback,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "discard policy must be require or zero-fallback",
                        ));
                    }
                };
                Ok(Command::Trim {
                    path,
                    selected: format(selected)?,
                    offset: byte_count(offset)?,
                    length: byte_count(length)?,
                    policy,
                    parents,
                })
            }
            [command, path, selected, offset, length, parents @ ..] if command == "zero" => {
                Ok(Command::Zero {
                    path,
                    selected: format(selected)?,
                    offset: byte_count(offset)?,
                    length: byte_count(length)?,
                    parents,
                })
            }
            [command, path, selected, parents @ ..] if command == "hash" => Ok(Command::Hash {
                path,
                selected: format(selected)?,
                parents,
            }),
            [command, path, selected, parents @ ..] if command == "map" => Ok(Command::Map {
                path,
                selected: format(selected)?,
                parents,
            }),
            [command, input, from, output, to, size, policy] if command == "resize" => {
                let size = size
                    .to_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "size must be an integer byte count",
                        )
                    })?;
                Ok(Command::Resize {
                    input,
                    from: format(from)?,
                    output,
                    to: format(to)?,
                    size,
                    policy: shrink_policy(policy)?,
                })
            }
            [command, access, path, selected, parents @ ..] if command == "capabilities" => {
                Ok(Command::Capabilities {
                    access: Access::parse(access)?,
                    path,
                    selected: format(selected)?,
                    parents,
                })
            }
            [command, path, selected, parents @ ..] if command == "info" => Ok(Command::Info {
                path,
                selected: format(selected)?,
                parents,
            }),
            [command, input, from, output, to] if command == "convert" => Ok(Command::Convert {
                input,
                from: format(from)?,
                output,
                to: format(to)?,
            }),
            [command, input, from, output, to] if command == "compact" => Ok(Command::Compact {
                input,
                from: format(from)?,
                output,
                to: format(to)?,
            }),
            [command, left, left_format, right, right_format] if command == "compare" => {
                Ok(Command::Compare {
                    left,
                    left_format: format(left_format)?,
                    right,
                    right_format: format(right_format)?,
                })
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: virtdisk graph info MANIFEST AUTHORIZED_IMAGE... | graph select-in-place MANIFEST STATE AUTHORIZED_IMAGE... | graph select MANIFEST STATE OUTPUT_MANIFEST AUTHORIZED_IMAGE... | graph rebase-generation MANIFEST SOURCE PARENT DIRECTORY AUTHORIZED_IMAGE... | graph flatten MANIFEST SOURCE OUTPUT FORMAT AUTHORIZED_IMAGE... | graph merge MANIFEST CHILD ANCESTOR OUTPUT FORMAT AUTHORIZED_IMAGE... | graph rebase MANIFEST SOURCE PARENT OUTPUT AUTHORIZED_IMAGE... | graph snapshot MANIFEST PARENT DIRECTORY FORMAT AUTHORIZED_IMAGE... | capabilities read|write IMAGE FORMAT [PARENT...] | check IMAGE FORMAT structure|payload [PARENT...] | snapshot create IMAGE ID NAME [PARENT...] | snapshot delete|revert IMAGE ID [PARENT...] | snapshot list IMAGE [PARENT...] | snapshot export IMAGE ID OUTPUT FORMAT [PARENT...] | create|create-sparse OUTPUT FORMAT SIZE | preallocate IMAGE FORMAT OFFSET LENGTH | info|hash|map IMAGE FORMAT [PARENT...] | convert|compact INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT | compare LEFT LEFT_FORMAT RIGHT RIGHT_FORMAT | resize INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT SIZE reject|zero-tail|allow-loss | resize-native IMAGE FORMAT SIZE reject|zero-tail|allow-loss [PARENT...] | zero IMAGE FORMAT OFFSET LENGTH [PARENT...] | trim IMAGE FORMAT OFFSET LENGTH require|zero-fallback [PARENT...]",
            )),
        }
    }
}

fn format(value: &OsStr) -> io::Result<ImageFormat> {
    match value.to_str() {
        Some("raw") => Ok(ImageFormat::Raw),
        Some("qcow2") => Ok(ImageFormat::Qcow2),
        Some("vdi") => Ok(ImageFormat::Vdi),
        Some("vmdk") => Ok(ImageFormat::Vmdk),
        Some("vhdx") => Ok(ImageFormat::Vhdx),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown format",
        )),
    }
}

fn snapshot_bytes(value: &OsStr) -> io::Result<Vec<u8>> {
    let value = value.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot bytes require UTF-8 or hex:HEX_BYTES",
        )
    })?;
    let Some(encoded) = value.strip_prefix("hex:") else {
        return Ok(value.as_bytes().to_vec());
    };
    if encoded.len() % 2 != 0 || encoded.len() > 131070 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid hexadecimal snapshot bytes",
        ));
    }
    encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            digit(pair[0])
                .zip(digit(pair[1]))
                .map(|(high, low)| high * 16 + low)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid hexadecimal snapshot bytes",
                    )
                })
        })
        .collect()
}

fn byte_count(value: &OsStr) -> io::Result<u64> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "range values must be integer byte counts",
            )
        })
}

fn shrink_policy(value: &OsStr) -> io::Result<ShrinkPolicy> {
    match value.to_str() {
        Some("reject") => Ok(ShrinkPolicy::Reject),
        Some("zero-tail") => Ok(ShrinkPolicy::RequireZero),
        Some("allow-loss") => Ok(ShrinkPolicy::AllowDataLoss),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shrink policy must be reject, zero-tail or allow-loss",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_paths_are_not_interpreted_as_options() {
        let args = ["info", "image", "qcow2", "--recover", "--progress"].map(OsString::from);
        let Command::Info { parents, .. } = Command::parse(&args).unwrap() else {
            panic!("expected info command");
        };
        assert_eq!(parents, &args[3..]);
    }

    #[cfg(unix)]
    #[test]
    fn image_paths_preserve_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let args = [
            OsString::from("info"),
            OsString::from_vec(b"image-\xff".to_vec()),
            OsString::from("raw"),
        ];
        let Command::Info { path, .. } = Command::parse(&args).unwrap() else {
            panic!("expected info command");
        };
        assert_eq!(path, args[1]);
    }

    #[test]
    fn snapshot_identifiers_decode_binary_hex_without_utf8_conversion() {
        let args = ["snapshot", "create", "image", "hex:00ff80", "hex:"].map(OsString::from);
        let Command::SnapshotCreate { id, name, .. } = Command::parse(&args).unwrap() else {
            panic!("expected snapshot creation command");
        };
        assert_eq!((id, name), (vec![0, 255, 128], vec![]));
    }

    #[test]
    fn preallocation_rejects_unexpected_dependency_arguments() {
        let args = ["preallocate", "image", "qcow2", "0", "512", "parent"].map(OsString::from);
        assert!(matches!(
            Command::parse(&args),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }
}
