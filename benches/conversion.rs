//! Diagnostic conversion costs; every timed conversion includes verification and publication.
use std::{fs::File, io::Write, time::Instant};
use virtdisk::{
    ConversionPolicy, ImageFormat, OperationContext, OperationEvent, OperationLimits,
    OperationProfiler, RawDisk, convert_image_with_policy_and_context,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let smoke = args.iter().any(|arg| arg == "--smoke");
    let option = |name: &str, fallback: u64| -> Result<u64, Box<dyn std::error::Error>> {
        match args.iter().position(|arg| arg == name) {
            Some(index) => Ok(args.get(index + 1).ok_or("missing option value")?.parse()?),
            None => Ok(fallback),
        }
    };
    let size = option("--size-mib", if smoke { 2 } else { 256 })?
        .checked_mul(1 << 20)
        .ok_or("size overflow")?;
    let samples = option("--samples", if smoke { 1 } else { 10 })?;
    if size == 0 || samples == 0 {
        return Err("size and samples must be positive".into());
    }
    let nonzero_run = option("--nonzero-run-kib", 64)?;
    if ![64, 1024].contains(&nonzero_run) {
        return Err("nonzero run must be 64 or 1024 KiB".into());
    }
    let streaming = args.iter().any(|arg| arg == "--streaming-allocation");
    let buffer = usize::try_from(
        option("--buffer-kib", 64)?
            .checked_mul(1024)
            .ok_or("buffer overflow")?,
    )?;
    let workers = usize::try_from(option("--workers", 1)?)?;
    let tuned = streaming
        || args
            .iter()
            .any(|arg| ["--buffer-kib", "--workers", "--memory-mib"].contains(&arg.as_str()));
    let memory = usize::try_from(
        option("--memory-mib", 128)?
            .checked_mul(1048576)
            .ok_or("memory overflow")?,
    )?;
    let limits = if tuned {
        OperationLimits::default().working_memory_bytes(memory)?
    } else {
        OperationLimits::default()
    };
    let formats = match args.iter().position(|arg| arg == "--formats") {
        Some(index) => args
            .get(index + 1)
            .ok_or("missing format list")?
            .split(',')
            .map(|format| match format {
                "raw" => Ok(ImageFormat::Raw),
                "qcow2" => Ok(ImageFormat::Qcow2),
                "vdi" => Ok(ImageFormat::Vdi),
                "vmdk" => Ok(ImageFormat::Vmdk),
                "vhdx" => Ok(ImageFormat::Vhdx),
                _ => Err("unsupported format filter"),
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![
            ImageFormat::Raw,
            ImageFormat::Qcow2,
            ImageFormat::Vdi,
            ImageFormat::Vmdk,
            ImageFormat::Vhdx,
        ],
    };
    let directory = tempfile::tempdir()?;
    println!(
        "format,data,nonzero_run_kib,policy,profiling,sample,streaming_allocation,buffer_bytes,workers,memory_bytes,event,calls,bytes,failures,seconds,logical_mib_per_second,container_bytes"
    );
    for sparse_data in [false, true] {
        let input = directory.path().join(if sparse_data {
            "sparse.raw"
        } else {
            "dense.raw"
        });
        let mut file = File::create(&input)?;
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut block = vec![0; 65536];
        for index in 0..size.div_ceil(block.len() as u64) {
            for byte in &mut block {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = if sparse_data && (index / (nonzero_run / 64)) % 16 != 0 {
                    0
                } else {
                    state as u8
                };
            }
            let count = (size - index * block.len() as u64).min(block.len() as u64) as usize;
            file.write_all(&block[..count])?;
        }
        file.sync_all()?;
        drop(file);
        let source = RawDisk::open(&input)?;
        for &format in &formats {
            for policy in [ConversionPolicy::Default, ConversionPolicy::Sparse] {
                for sample in 0..samples {
                    // Alternate pair order to expose, without claiming to remove, cache bias.
                    for enabled in if sample % 2 == 0 {
                        [false, true]
                    } else {
                        [true, false]
                    } {
                        let output = directory.path().join("output.img");
                        let profiler = OperationProfiler::default();
                        let mut context = if enabled {
                            OperationContext::new(limits).with_profiler(profiler.clone())
                        } else {
                            OperationContext::new(limits)
                        };
                        context = context
                            .with_streaming_allocation(streaming)
                            .with_buffer_bytes(buffer)?
                            .with_workers(workers)?;
                        let started = Instant::now();
                        convert_image_with_policy_and_context(
                            &source,
                            &output,
                            format,
                            policy,
                            &mut context,
                        )?;
                        let elapsed = started.elapsed().as_secs_f64();
                        let length = std::fs::metadata(&output)?.len();
                        let label = format!(
                            "{format:?},{},{nonzero_run},{policy:?},{enabled},{sample},{streaming},{buffer},{workers},{}",
                            if sparse_data { "6.25_percent" } else { "dense" },
                            if tuned { memory } else { 131072 }
                        );
                        println!(
                            "{label},total,1,{size},0,{elapsed:.9},{:.3},{length}",
                            size as f64 / 1048576.0 / elapsed
                        );
                        if enabled {
                            for event in [
                                OperationEvent::SourceRead,
                                OperationEvent::AllocationRead,
                                OperationEvent::ZeroDetection,
                                OperationEvent::PayloadWrite,
                                OperationEvent::MetadataWrite,
                                OperationEvent::Seek,
                                OperationEvent::Sync,
                                OperationEvent::OutputReopen,
                                OperationEvent::ByteVerification,
                                OperationEvent::MappingValidation,
                                OperationEvent::Publication,
                            ] {
                                let metric = profiler.metrics(event);
                                println!(
                                    "{label},{event:?},{},{},{},{:.9},unavailable,{length}",
                                    metric.calls,
                                    metric.bytes,
                                    metric.failures,
                                    metric.elapsed.as_secs_f64()
                                );
                            }
                        }
                        std::fs::remove_file(output)?;
                    }
                }
            }
        }
    }
    Ok(())
}
