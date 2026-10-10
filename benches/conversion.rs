//! Diagnostic conversion costs; every timed conversion includes verification and publication.
use std::{fs::File, io::Write, time::Instant};
use virtdisk::{
    ConversionPolicy, ImageFormat, OperationContext, OperationEvent, OperationProfiler, RawDisk,
    convert_image_with_policy_and_context,
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
    let directory = tempfile::tempdir()?;
    println!(
        "format,data,policy,profiling,sample,event,calls,bytes,failures,seconds,logical_mib_per_second,container_bytes"
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
                *byte = if sparse_data && index % 16 != 0 {
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
        for format in [
            ImageFormat::Raw,
            ImageFormat::Qcow2,
            ImageFormat::Vdi,
            ImageFormat::Vmdk,
            ImageFormat::Vhdx,
        ] {
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
                            OperationContext::default().with_profiler(profiler.clone())
                        } else {
                            OperationContext::default()
                        };
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
                            "{format:?},{},{policy:?},{enabled},{sample}",
                            if sparse_data { "6.25_percent" } else { "dense" }
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
