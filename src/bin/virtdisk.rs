//! Offline image inspection, conversion, and logical comparison.
use std::sync::Arc;
use std::{env, ffi::OsStr, io, path::PathBuf, process::ExitCode};
use virtdisk::{
    DiscardPolicy, DiscardResult, Image, ImageFormat, ImageWriter, InspectImage, ReadAt,
    ShrinkPolicy, WriteAt, compact_image, compare_images, convert_image, hash_image, resize_image,
};

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

fn run() -> io::Result<bool> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match args.as_slice() {
        [command, action, path, id]
            if command == "snapshot" && (action == "delete" || action == "revert") =>
        {
            let id = snapshot_bytes(id)?;
            let mut writer = ImageWriter::open(path, ImageFormat::Qcow2)?;
            let snapshot = if action == "delete" {
                writer.delete_snapshot(&id)?
            } else {
                writer.revert_snapshot(&id)?
            };
            writer.flush()?;
            println!(
                "{{\"id_hex\":\"{}\",\"virtual_size\":{}}}",
                hex(&snapshot.id),
                writer.len()
            );
            Ok(true)
        }
        [command, action, path, id, name] if command == "snapshot" && action == "create" => {
            let id = snapshot_bytes(id)?;
            let name = snapshot_bytes(name)?;
            let mut writer = ImageWriter::open(path, ImageFormat::Qcow2)?;
            let snapshot = writer.create_snapshot(&id, &name)?;
            writer.flush()?;
            println!(
                "{{\"id_hex\":\"{}\",\"virtual_size\":{}}}",
                hex(&snapshot.id),
                snapshot.virtual_size
            );
            Ok(true)
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
            let report = virtdisk::check_image(
                path,
                format(selected)?,
                &parents.iter().map(PathBuf::from).collect::<Vec<_>>(),
                options,
            )?;
            let scope = match report.structural {
                virtdisk::CheckScope::RawLengthOnly => "raw-length-only",
                virtdisk::CheckScope::SupportedContainerOwnership => {
                    "supported-container-ownership"
                }
            };
            println!(
                "{{\"virtual_size\":{},\"structural_scope\":\"{scope}\",\"payload_bytes_read\":{}}}",
                report.virtual_size, report.payload_bytes_read
            );
            Ok(true)
        }
        [command, action, path, parents @ ..] if command == "snapshot" && action == "list" => {
            let image = virtdisk::Qcow2::open_chain(
                path,
                &parents.iter().map(PathBuf::from).collect::<Vec<_>>(),
            )?;
            let snapshots = image.list_snapshots()?;
            print!("[");
            for (index, snapshot) in snapshots.iter().enumerate() {
                if index != 0 {
                    print!(",");
                }
                print!(
                    "{{\"id_hex\":\"{}\",\"name_hex\":\"{}\",\"virtual_size\":{},\"vm_state_size\":{},\"date_seconds\":{},\"date_nanoseconds\":{}}}",
                    hex(&snapshot.id),
                    hex(&snapshot.name),
                    snapshot.virtual_size,
                    snapshot.vm_state_size,
                    snapshot.date_seconds,
                    snapshot.date_nanoseconds
                );
            }
            println!("]");
            Ok(true)
        }
        [command, action, path, id, output, selected, parents @ ..]
            if command == "snapshot" && action == "export" =>
        {
            let id = snapshot_bytes(id)?;
            let image = Arc::new(virtdisk::Qcow2::open_chain(
                path,
                &parents.iter().map(PathBuf::from).collect::<Vec<_>>(),
            )?);
            let view = image.open_snapshot(&id)?;
            convert_image(&view, output, format(selected)?)?;
            Ok(true)
        }
        [command, path, selected, size, policy] if command == "resize-native" => {
            let size = byte_count(size)?;
            let policy = shrink_policy(policy)?;
            let mut writer = ImageWriter::open(path, format(selected)?)?;
            writer.resize(size, policy)?;
            writer.flush()?;
            println!("{{\"virtual_size\":{size}}}");
            Ok(true)
        }
        [command, path, selected, offset, length] if command == "preallocate" => {
            let offset = byte_count(offset)?;
            let length = byte_count(length)?;
            let writer = ImageWriter::open(path, format(selected)?)?;
            writer.preallocate(offset, length)?;
            writer.flush()?;
            println!("{{\"offset\":{offset},\"length\":{length},\"method\":\"preallocated\"}}");
            Ok(true)
        }
        [command, path, selected, size] if command == "create" || command == "create-sparse" => {
            let size = byte_count(size)?;
            let writer = if command == "create-sparse" {
                ImageWriter::create_sparse(path, format(selected)?, size)?
            } else {
                ImageWriter::create(path, format(selected)?, size)?
            };
            writer.flush()?;
            println!("{{\"virtual_size\":{size}}}");
            Ok(true)
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
            let offset = byte_count(offset)?;
            let length = byte_count(length)?;
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
            let writer = open_writer(path, selected, parents)?;
            let result = writer.discard(offset, length, policy)?;
            writer.flush()?;
            let method = match result {
                DiscardResult::Deallocated => "deallocated",
                DiscardResult::Zeroed => "zeroed",
            };
            println!("{{\"offset\":{offset},\"length\":{length},\"method\":\"{method}\"}}");
            Ok(true)
        }
        [command, path, selected, offset, length, parents @ ..] if command == "zero" => {
            let offset = byte_count(offset)?;
            let length = byte_count(length)?;
            let writer = open_writer(path, selected, parents)?;
            writer.write_zeroes(offset, length)?;
            writer.flush()?;
            println!("{{\"offset\":{offset},\"length\":{length},\"method\":\"zeroed\"}}");
            Ok(true)
        }
        [command, path, selected, parents @ ..] if command == "hash" => {
            let image = open_reader(path, selected, parents)?;
            for byte in hash_image(&image)? {
                print!("{byte:02x}");
            }
            println!();
            Ok(true)
        }
        [command, path, selected, parents @ ..] if command == "map" => {
            let image = open_reader(path, selected, parents)?;
            image.visit_extents(&mut |extent| {
                let kind = match extent.kind {
                    virtdisk::ExtentKind::Allocated => "allocated",
                    virtdisk::ExtentKind::Zero => "zero",
                    virtdisk::ExtentKind::Inherited => "inherited",
                    virtdisk::ExtentKind::Unknown => "unknown",
                };
                println!(
                    "{{\"offset\":{},\"length\":{},\"kind\":\"{kind}\"}}",
                    extent.offset, extent.length
                );
                Ok(())
            })?;
            Ok(true)
        }
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
            let policy = shrink_policy(policy)?;
            resize_image(
                &Image::open(input, Some(format(from)?))?,
                output,
                format(to)?,
                size,
                policy,
            )?;
            Ok(true)
        }
        [command, path, selected, parents @ ..] if command == "info" => {
            let image = open_reader(path, selected, parents)?;
            let info = image.info();
            let inspection = image.inspection();
            let geometry = inspection.geometry;
            let number =
                |value: Option<u64>| value.map_or_else(|| "null".to_owned(), |n| n.to_string());
            let writable = inspection.capabilities.get(virtdisk::ImageOperation::Write)
                == virtdisk::Capability::Supported;
            let family = match info.format {
                ImageFormat::Raw => "raw",
                ImageFormat::Qcow2 => "qcow2",
                ImageFormat::Vdi => "vdi",
                ImageFormat::Vmdk => "vmdk",
                ImageFormat::Vhdx => "vhdx",
            };
            println!(
                "{{\"format\":\"{family}\",\"virtual_size\":{},\"container_size\":{},\"logical_sector_size\":{},\"physical_sector_size\":{},\"allocation_block_size\":{},\"write_supported\":{writable},\"has_parent\":{},\"native_snapshots\":{}}}",
                info.virtual_size,
                info.container_size,
                number(geometry.logical_sector_size.map(u64::from)),
                number(geometry.physical_sector_size.map(u64::from)),
                number(geometry.allocation_block_size),
                inspection.has_parent,
                number(inspection.native_snapshots.map(u64::from))
            );
            Ok(true)
        }
        [command, input, from, output, to] if command == "convert" => {
            let image = Image::open(input, Some(format(from)?))?;
            convert_image(&image, output, format(to)?)?;
            Ok(true)
        }
        [command, input, from, output, to] if command == "compact" => {
            compact_image(
                &Image::open(input, Some(format(from)?))?,
                output,
                format(to)?,
            )?;
            Ok(true)
        }
        [command, left, left_format, right, right_format] if command == "compare" => {
            compare_images(
                &Image::open(left, Some(format(left_format)?))?,
                &Image::open(right, Some(format(right_format)?))?,
            )
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: virtdisk check IMAGE FORMAT structure|payload [PARENT...] | snapshot create IMAGE ID NAME | snapshot delete|revert IMAGE ID | snapshot list IMAGE [PARENT...] | snapshot export IMAGE ID OUTPUT FORMAT [PARENT...] | create|create-sparse OUTPUT FORMAT SIZE | preallocate IMAGE FORMAT OFFSET LENGTH | info|hash|map IMAGE FORMAT [PARENT...] | convert|compact INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT | compare LEFT LEFT_FORMAT RIGHT RIGHT_FORMAT | resize INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT SIZE reject|zero-tail|allow-loss | resize-native IMAGE FORMAT SIZE reject|zero-tail|allow-loss | zero IMAGE FORMAT OFFSET LENGTH [PARENT...] | trim IMAGE FORMAT OFFSET LENGTH require|zero-fallback [PARENT...]",
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

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
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

fn open_reader(
    path: &OsStr,
    selected: &OsStr,
    parents: &[std::ffi::OsString],
) -> io::Result<Image> {
    let selected = Some(format(selected)?);
    if parents.is_empty() {
        Image::open(path, selected)
    } else {
        Image::open_chain(
            path,
            selected,
            &parents.iter().map(PathBuf::from).collect::<Vec<_>>(),
        )
    }
}

fn open_writer(
    path: &OsStr,
    selected: &OsStr,
    parents: &[std::ffi::OsString],
) -> io::Result<ImageWriter> {
    let selected = format(selected)?;
    if parents.is_empty() {
        ImageWriter::open(path, selected)
    } else {
        ImageWriter::open_chain(
            path,
            selected,
            &parents.iter().map(PathBuf::from).collect::<Vec<_>>(),
        )
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("virtdisk: {error}");
            ExitCode::from(2)
        }
    }
}
