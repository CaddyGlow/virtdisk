//! Offline image inspection, conversion, and logical comparison.
#[path = "virtdisk/capabilities_output.rs"]
mod capabilities_output;
#[path = "virtdisk/error_output.rs"]
mod error_output;
#[path = "virtdisk/graph_output.rs"]
mod graph_output;
use std::sync::Arc;
use std::{env, ffi::OsStr, io, path::PathBuf, process::ExitCode};
use std::{io::Write as _, ops::ControlFlow};
use virtdisk::{
    DiscardPolicy, DiscardResult, Image, ImageFormat, ImageWriter, InspectImage, OperationContext,
    OperationLimits, ParserLimits, ReadAt, ReadRecoveryPolicy, ReaderOpenOptions, RecoveryPolicy,
    ShrinkPolicy, WriteAt, WriterOpenOptions, compact_image_with_context,
    compare_images_with_context, convert_image, convert_image_with_context,
    hash_image_with_context, resize_image_with_context, zero_image_with_context,
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

fn run(args: &[std::ffi::OsString]) -> io::Result<bool> {
    let (controls, args) = reader_controls(args)?;
    let reader_options = controls.reader;
    let mut progress_error = None;
    let mut observer = |progress| match write_progress(progress) {
        Ok(()) => ControlFlow::Continue(()),
        Err(error) => {
            progress_error = Some(error);
            ControlFlow::Break(())
        }
    };
    let result = {
        let mut context = OperationContext::new(controls.operation);
        if controls.progress {
            context = context.with_observer(&mut observer);
        }
        let (recovery, args) = match args {
            [flag, rest @ ..] if flag == "--recover" => {
                let mutation = match rest {
                    [command, ..]
                        if matches!(
                            command.to_str(),
                            Some("zero" | "trim" | "preallocate" | "resize-native")
                        ) =>
                    {
                        true
                    }
                    [command, action, ..]
                        if command == "snapshot"
                            && matches!(action.to_str(), Some("create" | "delete" | "revert")) =>
                    {
                        true
                    }
                    _ => false,
                };
                if !mutation {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--recover requires an existing-image mutation command",
                    ));
                }
                (RecoveryPolicy::Recover, rest)
            }
            args => (RecoveryPolicy::RejectPending, args),
        };
        run_command(
            args,
            &reader_options,
            recovery,
            controls.execution,
            &mut context,
        )
    };
    match progress_error {
        Some(error) => Err(error),
        None => result,
    }
}

fn write_progress(progress: virtdisk::OperationProgress) -> io::Result<()> {
    let phase = match progress.phase {
        virtdisk::OperationPhase::Processing => "processing",
        virtdisk::OperationPhase::AllocationScan => "allocation-scan",
        virtdisk::OperationPhase::ImageExport => "image-export",
        virtdisk::OperationPhase::OutputVerification => "output-verification",
        virtdisk::OperationPhase::TailValidation => "tail-validation",
        virtdisk::OperationPhase::Publication => "publication",
        virtdisk::OperationPhase::GenerationPublication => "generation-publication",
        virtdisk::OperationPhase::MetadataValidation => "metadata-validation",
        virtdisk::OperationPhase::PayloadValidation => "payload-validation",
        virtdisk::OperationPhase::Zeroing => "zeroing",
        virtdisk::OperationPhase::SnapshotDeletion => "snapshot-deletion",
        virtdisk::OperationPhase::NativeResize => "native-resize",
        virtdisk::OperationPhase::NativeDiscard => "native-discard",
        virtdisk::OperationPhase::NativePreallocation => "native-preallocation",
        virtdisk::OperationPhase::ManifestReplacement => "manifest-replacement",
        virtdisk::OperationPhase::NativeSnapshotCreation => "native-snapshot-creation",
        virtdisk::OperationPhase::NativeSnapshotDeletion => "native-snapshot-deletion",
        virtdisk::OperationPhase::NativeSnapshotRevert => "native-snapshot-revert",
        _ => "unknown",
    };
    writeln!(
        io::stderr().lock(),
        "{{\"type\":\"progress\",\"phase\":\"{phase}\",\"completed_bytes\":{},\"total_bytes\":{},\"logical_bytes\":{},\"io_operations\":{},\"peak_scratch_bytes\":{}}}",
        progress.completed_bytes,
        progress.total_bytes,
        progress.usage.logical_bytes,
        progress.usage.io_operations,
        progress.usage.peak_scratch_bytes
    )
}

fn run_command(
    args: &[std::ffi::OsString],
    reader_options: &ReaderOpenOptions,
    recovery: RecoveryPolicy,
    execution: MutationExecution,
    context: &mut OperationContext<'_>,
) -> io::Result<bool> {
    match args {
        [
            command,
            action,
            manifest,
            parent,
            directory,
            family,
            authorized @ ..,
        ] if command == "graph" && action == "snapshot" => {
            let family = format(family)?;
            let mut graph = open_graph(manifest, authorized, reader_options)?;
            let published =
                graph.snapshot_generation_with_context(parent, directory, family, context)?;
            writeln!(
                io::stdout().lock(),
                "{{\"method\":\"external-snapshot-generation\",\"image\":\"image\",\"manifest\":\"graph.manifest\",\"images\":{}}}",
                published.images().len()
            )?;
            Ok(true)
        }
        [
            command,
            action,
            manifest,
            source,
            output,
            family,
            authorized @ ..,
        ] if command == "graph" && action == "flatten" => {
            let family = format(family)?;
            let graph = open_graph(manifest, authorized, reader_options)?;
            graph.flatten_with_context(source, output, family, context)?;
            writeln!(io::stdout().lock(), "{{\"method\":\"flatten\"}}")?;
            Ok(true)
        }
        [
            command,
            action,
            manifest,
            source,
            ancestor,
            output,
            family,
            authorized @ ..,
        ] if command == "graph" && action == "merge" => {
            let family = format(family)?;
            let graph = open_graph(manifest, authorized, reader_options)?;
            graph.merge_to_with_context(source, ancestor, output, family, context)?;
            writeln!(io::stdout().lock(), "{{\"method\":\"new-output-merge\"}}")?;
            Ok(true)
        }
        [
            command,
            action,
            manifest,
            source,
            parent,
            output,
            authorized @ ..,
        ] if command == "graph" && (action == "rebase" || action == "rebase-generation") => {
            let mut graph = open_graph(manifest, authorized, reader_options)?;
            if action == "rebase-generation" {
                let published =
                    graph.rebase_generation_with_context(source, parent, output, context)?;
                writeln!(
                    io::stdout().lock(),
                    "{{\"method\":\"rebase-generation\",\"image\":\"image\",\"manifest\":\"graph.manifest\",\"images\":{}}}",
                    published.images().len()
                )?;
                return Ok(true);
            }
            graph.rebase_to_with_context(source, parent, output, context)?;
            writeln!(
                io::stdout().lock(),
                "{{\"method\":\"new-output-rebase\",\"format\":\"qcow2\"}}"
            )?;
            Ok(true)
        }
        [command, action, path, selected, authorized @ ..]
            if command == "graph" && action == "select-in-place" =>
        {
            let expected = virtdisk::GraphManifest::open(path)?;
            let graph = open_graph_declaration(&expected, authorized, reader_options)?;
            let next = graph.manifest(Some(std::path::Path::new(selected)))?;
            next.replace_with_context(path, &expected, context)?;
            writeln!(
                io::stdout().lock(),
                "{{\"method\":\"select-in-place\",\"images\":{}}}",
                next.images().len()
            )?;
            Ok(true)
        }
        [command, action, manifest, selected, output, authorized @ ..]
            if command == "graph" && action == "select" =>
        {
            let graph = open_graph(manifest, authorized, reader_options)?;
            let manifest = graph.save_manifest_with_context(
                Some(std::path::Path::new(selected)),
                output,
                context,
            )?;
            writeln!(
                io::stdout().lock(),
                "{{\"method\":\"select\",\"images\":{}}}",
                manifest.images().len()
            )?;
            Ok(true)
        }
        [command, action, manifest, authorized @ ..] if command == "graph" && action == "info" => {
            let manifest = virtdisk::GraphManifest::open(manifest)?;
            let graph = open_graph_declaration(&manifest, authorized, reader_options)?;
            graph_output::write_json(&mut io::stdout().lock(), &manifest, &graph)?;
            Ok(true)
        }
        [command, action, path, id, parents @ ..]
            if command == "snapshot" && (action == "delete" || action == "revert") =>
        {
            let id = snapshot_bytes(id)?;
            let mut writer = open_writer(path, OsStr::new("qcow2"), parents, recovery)?;
            let snapshot = match execution {
                MutationExecution::WholeCall if action == "delete" => {
                    writer.delete_snapshot(&id)?
                }
                MutationExecution::WholeCall => writer.revert_snapshot(&id)?,
                MutationExecution::Bounded if action == "delete" => {
                    writer.delete_snapshot_with_context(&id, context)?
                }
                MutationExecution::Bounded => writer.revert_snapshot_with_context(&id, context)?,
            };
            writer.flush()?;
            println!(
                "{{\"id_hex\":\"{}\",\"virtual_size\":{}}}",
                hex(&snapshot.id),
                writer.len()
            );
            Ok(true)
        }
        [command, action, path, id, name, parents @ ..]
            if command == "snapshot" && action == "create" =>
        {
            let id = snapshot_bytes(id)?;
            let name = snapshot_bytes(name)?;
            let mut writer = open_writer(path, OsStr::new("qcow2"), parents, recovery)?;
            let snapshot = match execution {
                MutationExecution::WholeCall => writer.create_snapshot(&id, &name)?,
                MutationExecution::Bounded => {
                    writer.create_snapshot_with_context(&id, &name, context)?
                }
            };
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
            let report = virtdisk::check_image_with_limits_and_context(
                path,
                format(selected)?,
                &parents.iter().map(PathBuf::from).collect::<Vec<_>>(),
                options,
                reader_options.limits(),
                context,
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
            let image = virtdisk::Qcow2::open_chain_with_limits(
                path,
                &parents.iter().map(PathBuf::from).collect::<Vec<_>>(),
                reader_options.limits(),
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
            let image = Arc::new(virtdisk::Qcow2::open_chain_with_limits(
                path,
                &parents.iter().map(PathBuf::from).collect::<Vec<_>>(),
                reader_options.limits(),
            )?);
            let view = image.open_snapshot(&id)?;
            convert_image(&view, output, format(selected)?)?;
            Ok(true)
        }
        [command, path, selected, size, policy, parents @ ..] if command == "resize-native" => {
            let size = byte_count(size)?;
            let policy = shrink_policy(policy)?;
            let mut writer = open_writer(path, selected, parents, recovery)?;
            match execution {
                MutationExecution::WholeCall => writer.resize(size, policy)?,
                MutationExecution::Bounded => writer.resize_with_context(size, policy, context)?,
            }
            writer.flush()?;
            println!("{{\"virtual_size\":{size}}}");
            Ok(true)
        }
        [command, path, selected, offset, length] if command == "preallocate" => {
            let offset = byte_count(offset)?;
            let length = byte_count(length)?;
            let writer = open_writer(path, selected, &[], recovery)?;
            match execution {
                MutationExecution::WholeCall => writer.preallocate(offset, length)?,
                MutationExecution::Bounded => {
                    writer.preallocate_with_context(offset, length, context)?
                }
            }
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
            let writer = open_writer(path, selected, parents, recovery)?;
            let result = match execution {
                MutationExecution::WholeCall => writer.discard(offset, length, policy)?,
                MutationExecution::Bounded => {
                    writer.discard_with_context(offset, length, policy, context)?
                }
            };
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
            let writer = open_writer(path, selected, parents, recovery)?;
            match execution {
                MutationExecution::WholeCall => writer.write_zeroes(offset, length)?,
                MutationExecution::Bounded => {
                    zero_image_with_context(&writer, offset, length, context)?
                }
            }
            writer.flush()?;
            println!("{{\"offset\":{offset},\"length\":{length},\"method\":\"zeroed\"}}");
            Ok(true)
        }
        [command, path, selected, parents @ ..] if command == "hash" => {
            let image = open_reader(path, selected, parents, reader_options)?;
            for byte in hash_image_with_context(&image, context)? {
                print!("{byte:02x}");
            }
            println!();
            Ok(true)
        }
        [command, path, selected, parents @ ..] if command == "map" => {
            let image = open_reader(path, selected, parents, reader_options)?;
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
            resize_image_with_context(
                &open_reader(input, from, &[], reader_options)?,
                output,
                format(to)?,
                size,
                policy,
                context,
            )?;
            Ok(true)
        }
        [command, access, path, selected, parents @ ..] if command == "capabilities" => {
            match access.to_str() {
                Some("read") => capabilities_output::write_json(
                    &mut io::stdout().lock(),
                    open_reader(path, selected, parents, reader_options)?.inspection(),
                )?,
                Some("write") => {
                    let writer =
                        open_writer(path, selected, parents, RecoveryPolicy::RejectPending)?;
                    capabilities_output::write_json(&mut io::stdout().lock(), writer.inspection())?;
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "capabilities access must be read or write",
                    ));
                }
            }
            Ok(true)
        }
        [command, path, selected, parents @ ..] if command == "info" => {
            let image = open_reader(path, selected, parents, reader_options)?;
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
                "{{\"format\":\"{family}\",\"virtual_size\":{},\"container_size\":{},\"container_set_size\":{},\"logical_sector_size\":{},\"physical_sector_size\":{},\"allocation_block_size\":{},\"write_supported\":{writable},\"has_parent\":{},\"native_snapshots\":{}}}",
                info.virtual_size,
                info.container_size,
                number(inspection.container_set_size),
                number(geometry.logical_sector_size.map(u64::from)),
                number(geometry.physical_sector_size.map(u64::from)),
                number(geometry.allocation_block_size),
                inspection.has_parent,
                number(inspection.native_snapshots.map(u64::from))
            );
            Ok(true)
        }
        [command, input, from, output, to] if command == "convert" => {
            let image = open_reader(input, from, &[], reader_options)?;
            convert_image_with_context(&image, output, format(to)?, context)?;
            Ok(true)
        }
        [command, input, from, output, to] if command == "compact" => {
            compact_image_with_context(
                &open_reader(input, from, &[], reader_options)?,
                output,
                format(to)?,
                context,
            )?;
            Ok(true)
        }
        [command, left, left_format, right, right_format] if command == "compare" => {
            compare_images_with_context(
                &open_reader(left, left_format, &[], reader_options)?,
                &open_reader(right, right_format, &[], reader_options)?,
                context,
            )
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: virtdisk graph info MANIFEST AUTHORIZED_IMAGE... | graph select-in-place MANIFEST STATE AUTHORIZED_IMAGE... | graph select MANIFEST STATE OUTPUT_MANIFEST AUTHORIZED_IMAGE... | graph rebase-generation MANIFEST SOURCE PARENT DIRECTORY AUTHORIZED_IMAGE... | graph flatten MANIFEST SOURCE OUTPUT FORMAT AUTHORIZED_IMAGE... | graph merge MANIFEST CHILD ANCESTOR OUTPUT FORMAT AUTHORIZED_IMAGE... | graph rebase MANIFEST SOURCE PARENT OUTPUT AUTHORIZED_IMAGE... | graph snapshot MANIFEST PARENT DIRECTORY FORMAT AUTHORIZED_IMAGE... | capabilities read|write IMAGE FORMAT [PARENT...] | check IMAGE FORMAT structure|payload [PARENT...] | snapshot create IMAGE ID NAME [PARENT...] | snapshot delete|revert IMAGE ID [PARENT...] | snapshot list IMAGE [PARENT...] | snapshot export IMAGE ID OUTPUT FORMAT [PARENT...] | create|create-sparse OUTPUT FORMAT SIZE | preallocate IMAGE FORMAT OFFSET LENGTH | info|hash|map IMAGE FORMAT [PARENT...] | convert|compact INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT | compare LEFT LEFT_FORMAT RIGHT RIGHT_FORMAT | resize INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT SIZE reject|zero-tail|allow-loss | resize-native IMAGE FORMAT SIZE reject|zero-tail|allow-loss [PARENT...] | zero IMAGE FORMAT OFFSET LENGTH [PARENT...] | trim IMAGE FORMAT OFFSET LENGTH require|zero-fallback [PARENT...]",
        )),
    }
}

fn open_graph(
    manifest: &OsStr,
    authorized: &[std::ffi::OsString],
    options: &ReaderOpenOptions,
) -> io::Result<virtdisk::ImageGraph> {
    open_graph_declaration(
        &virtdisk::GraphManifest::open(manifest)?,
        authorized,
        options,
    )
}

fn open_graph_declaration(
    manifest: &virtdisk::GraphManifest,
    authorized: &[std::ffi::OsString],
    options: &ReaderOpenOptions,
) -> io::Result<virtdisk::ImageGraph> {
    if authorized.len() > 128 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "graph authorization exceeds 128 paths",
        ));
    }
    let mut paths = Vec::new();
    paths
        .try_reserve_exact(authorized.len())
        .map_err(io::Error::other)?;
    paths.extend(authorized.iter().map(PathBuf::from));
    manifest.open_graph_with_limits(&paths, options.limits())
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
    options: &ReaderOpenOptions,
) -> io::Result<Image> {
    let mut options = options.clone().format(format(selected)?);
    if !parents.is_empty() {
        options = options.authorized_paths(parents.iter().map(PathBuf::from));
    }
    Image::open_with_options(path, &options)
}

/// Parse only leading reader controls; never reinterpret dependency paths.
#[derive(Clone, Copy)]
enum MutationExecution {
    WholeCall,
    Bounded,
}

struct CliControls {
    execution: MutationExecution,
    reader: ReaderOpenOptions,
    operation: OperationLimits,
    progress: bool,
}

fn reader_controls(
    args: &[std::ffi::OsString],
) -> io::Result<(CliControls, &[std::ffi::OsString])> {
    let mut args = args;
    let mut limits = ParserLimits::default();
    let mut specified = false;
    let mut recovery = ReadRecoveryPolicy::RejectPending;
    let mut operation_specified = false;
    let mut operation_bytes = u64::MAX;
    let mut operation_io = u64::MAX;
    let mut operation_scratch = 131072u64;
    let mut progress = false;
    loop {
        if let [flag, rest @ ..] = args
            && flag == "--progress"
        {
            progress = true;
            operation_specified = true;
            args = rest;
            continue;
        }
        if args
            .first()
            .is_some_and(|argument| argument == "--operation-limit")
        {
            let [_, value, rest @ ..] = args else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--operation-limit requires NAME=INTEGER",
                ));
            };
            let (name, value) = value
                .to_str()
                .and_then(|value| value.split_once('='))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "operation limit requires NAME=INTEGER",
                    )
                })?;
            let value = value.parse::<u64>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "operation limit requires an integer",
                )
            })?;
            let field = match name {
                "bytes" => &mut operation_bytes,
                "io" => &mut operation_io,
                "scratch" => {
                    let value = usize::try_from(value).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "operation scratch limit exceeds platform size",
                        )
                    })?;
                    OperationLimits::default().scratch_bytes(value)?;
                    &mut operation_scratch
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unknown operation limit name",
                    ));
                }
            };
            *field = (*field).min(value);
            operation_specified = true;
            args = rest;
            continue;
        }
        if let [flag, rest @ ..] = args
            && flag == "--replay-vhdx-log"
        {
            recovery = ReadRecoveryPolicy::ReplayVhdxLog;
            specified = true;
            args = rest;
            continue;
        }
        if !args
            .first()
            .is_some_and(|argument| argument == "--parser-limit")
        {
            break;
        }
        let [_, value, rest @ ..] = args else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--parser-limit requires NAME=INTEGER",
            ));
        };
        let (name, value) = value
            .to_str()
            .and_then(|value| value.split_once('='))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "parser limit requires NAME=INTEGER",
                )
            })?;
        let value = value.parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "parser limit requires an integer",
            )
        })?;
        let hard = ParserLimits::default();
        let (field, ceiling) = match name {
            "metadata" => (&mut limits.metadata_bytes, hard.metadata_bytes),
            "cache" => (&mut limits.cache_bytes, hard.cache_bytes),
            "recursion" => (&mut limits.recursion_depth, hard.recursion_depth),
            "work" => (&mut limits.work_items, hard.work_items),
            "decompressed" => (&mut limits.decompressed_bytes, hard.decompressed_bytes),
            "decompression-buffer" => (
                &mut limits.decompression_buffer_bytes,
                hard.decompression_buffer_bytes,
            ),
            "attribute" => (&mut limits.attribute_bytes, hard.attribute_bytes),
            "attribute-list-records" => (
                &mut limits.attribute_list_records,
                hard.attribute_list_records,
            ),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unknown parser limit name",
                ));
            }
        };
        if value == 0 || value > ceiling {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} parser limit must be positive and at most {ceiling}"),
            ));
        }
        // Repetition can only tighten a previously specified ceiling.
        *field = (*field).min(value);
        specified = true;
        args = rest;
    }
    let snapshot_reader = matches!(args, [command, action, ..] if command == "snapshot" && matches!(action.to_str(), Some("list" | "export")));
    let graph_reader =
        matches!(args, [command, action, ..] if command == "graph" && action == "info");
    let graph_operation = matches!(args, [command, action, ..] if command == "graph" && matches!(action.to_str(), Some("snapshot" | "flatten" | "merge" | "rebase" | "rebase-generation" | "select" | "select-in-place")));
    let capability_reader =
        matches!(args, [command, access, ..] if command == "capabilities" && access == "read");
    let operation_args = match args {
        [flag, rest @ ..] if flag == "--recover" => rest,
        args => args,
    };
    let snapshot_operation = matches!(operation_args, [command, action, ..]
        if command == "snapshot" && matches!(action.to_str(), Some("create" | "delete" | "revert")));
    if operation_specified
        && !snapshot_operation
        && !graph_operation
        && !operation_args.first().is_some_and(|command| {
            matches!(
                command.to_str(),
                Some(
                    "hash"
                        | "compare"
                        | "check"
                        | "convert"
                        | "compact"
                        | "resize"
                        | "resize-native"
                        | "trim"
                        | "preallocate"
                        | "zero"
                )
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "operation controls require hash, compare, check, convert, compact, resize, resize-native, trim, preallocate, zero, snapshot create/delete/revert or graph snapshot/flatten/merge/rebase/rebase-generation/select/select-in-place",
        ));
    }
    if specified
        && !graph_reader
        && !graph_operation
        && !snapshot_reader
        && !capability_reader
        && !args.first().is_some_and(|command| {
            matches!(
                command.to_str(),
                Some(
                    "info"
                        | "hash"
                        | "map"
                        | "convert"
                        | "compact"
                        | "compare"
                        | "resize"
                        | "check"
                )
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reader parser/replay controls require info, hash, map, convert, compact, compare, resize, check or snapshot list/export or capabilities read or graph snapshot/flatten/merge/rebase/rebase-generation/select/select-in-place or graph info",
        ));
    }
    if recovery != ReadRecoveryPolicy::RejectPending
        && (graph_reader
            || graph_operation
            || snapshot_reader
            || args.first().is_some_and(|command| command == "check"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "check, snapshot and graph parser controls do not authorize log replay",
        ));
    }
    Ok((
        CliControls {
            execution: if operation_specified {
                MutationExecution::Bounded
            } else {
                MutationExecution::WholeCall
            },
            progress,
            reader: ReaderOpenOptions::default()
                .parser_limits(limits)?
                .recovery_policy(recovery),
            operation: OperationLimits::default()
                .logical_bytes(operation_bytes)
                .io_operations(operation_io)
                .scratch_bytes(operation_scratch as usize)?,
        },
        args,
    ))
}

fn open_writer(
    path: &OsStr,
    selected: &OsStr,
    parents: &[std::ffi::OsString],
    recovery: RecoveryPolicy,
) -> io::Result<ImageWriter> {
    let selected = format(selected)?;
    let mut options = WriterOpenOptions::default().recovery_policy(recovery);
    if !parents.is_empty() {
        options = options.authorized_paths(parents.iter().map(PathBuf::from));
    }
    ImageWriter::open_with_options(path, selected, &options)
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let (json_errors, args) = match args.as_slice() {
        [flag, rest @ ..] if flag == "--json-errors" => (true, rest),
        args => (false, args),
    };
    match run(args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            if json_errors {
                let _ = error_output::write_json(&mut io::stderr().lock(), &error);
            } else {
                let _ = writeln!(io::stderr().lock(), "virtdisk: {error}");
            }
            ExitCode::from(2)
        }
    }
}
