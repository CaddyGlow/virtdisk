//! Warm-cache lifecycle timings with exact whole-image verification.
use std::{path::Path, time::Instant};
use virtdisk::{
    DiscardPolicy, Image, ImageFormat, ImageWriter, ReadAt, RecoveryPolicy, ShrinkPolicy, WriteAt,
    WriterOpenOptions, compact_image,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn verify(path: &Path, format: ImageFormat, expected: &[u8]) -> Result<()> {
    let image = Image::open(path, Some(format))?;
    if image.len() != expected.len() as u64 {
        return Err("logical length mismatch".into());
    }
    let mut buffer = vec![0; 1 << 20];
    for (index, bytes) in expected.chunks(buffer.len()).enumerate() {
        image.read_exact_at((index * buffer.len()) as u64, &mut buffer[..bytes.len()])?;
        if &buffer[..bytes.len()] != bytes {
            return Err(format!("logical byte mismatch at chunk {index}").into());
        }
    }
    Ok(())
}

fn verify_writer(writer: &ImageWriter, expected: &[u8]) -> Result<()> {
    if writer.len() != expected.len() as u64 {
        return Err("writer length mismatch".into());
    }
    let mut buffer = vec![0; 1 << 20];
    for (index, bytes) in expected.chunks(buffer.len()).enumerate() {
        writer.read_exact_at((index * buffer.len()) as u64, &mut buffer[..bytes.len()])?;
        if &buffer[..bytes.len()] != bytes {
            return Err("writer logical byte mismatch".into());
        }
    }
    Ok(())
}

fn report(
    format: ImageFormat,
    phase: &str,
    sample: usize,
    bytes: u64,
    seconds: f64,
    latency: &mut [u128],
    path: &Path,
) -> Result<()> {
    latency.sort_unstable();
    let percentile = |percent: usize| {
        if latency.len() >= 1000 {
            latency[(latency.len() * percent).div_ceil(100) - 1].to_string()
        } else {
            "unavailable".into()
        }
    };
    let metadata = std::fs::metadata(path)?;
    #[cfg(unix)]
    let allocated = {
        use std::os::unix::fs::MetadataExt;
        (metadata.blocks() * 512).to_string()
    };
    #[cfg(not(unix))]
    let allocated = "unavailable";
    println!(
        "{format:?},{phase},{sample},{bytes},{seconds:.9},{},{},{},{},{allocated},unavailable,warm",
        latency.len(),
        percentile(95),
        percentile(99),
        metadata.len()
    );
    Ok(())
}

fn requests(
    writer: &ImageWriter,
    count: usize,
    partial: bool,
    value: u8,
    expected: &mut [u8],
) -> Result<Vec<u128>> {
    let payload = vec![value; if partial { 512 } else { 4096 }];
    let mut latency = Vec::with_capacity(count);
    for index in 0..count {
        let offset = index * 4096 + if partial { 511 } else { 0 };
        let start = Instant::now();
        writer.write_all_at(offset as u64, &payload)?;
        latency.push(start.elapsed().as_nanos());
        expected[offset..offset + payload.len()].copy_from_slice(&payload);
    }
    Ok(latency)
}

fn main() -> Result<()> {
    let mut size: usize = 16 << 20;
    let mut count = 1000;
    let mut samples = 10;
    let mut filter = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--bench" => {}
            "--smoke" => {
                size = 4 << 20;
                count = 16;
                samples = 1;
            }
            "--size" => size = args.next().ok_or("missing size")?.parse()?,
            "--requests" => count = args.next().ok_or("missing requests")?.parse()?,
            "--samples" => samples = args.next().ok_or("missing samples")?.parse()?,
            "--format" => filter = Some(args.next().ok_or("missing format")?),
            _ => return Err(format!("unknown argument {argument}").into()),
        }
    }
    if !(4 << 20..=256 << 20).contains(&size)
        || !size.is_multiple_of(1 << 20)
        || count == 0
        || count > size / 4096
        || samples == 0
    {
        return Err(
            "size must be 4..256 MiB in MiB units; requests must fit; samples positive".into(),
        );
    }
    if filter.as_ref().is_some_and(|filter: &String| {
        !["raw", "qcow2", "vdi", "vmdk", "vhdx"].contains(&filter.as_str())
    }) {
        return Err("unknown format".into());
    }
    let directory = tempfile::tempdir()?;
    let timer = Instant::now();
    for _ in 0..10000 {
        std::hint::black_box(Instant::now().elapsed());
    }
    eprintln!(
        "timer_pair_ns={}; flush per phase; physical I/O amplification unavailable; clean recovery-policy open is not crash replay",
        timer.elapsed().as_nanos() / 10000
    );
    println!(
        "format,phase,sample,logical_request_bytes,seconds,latency_samples,p95_ns,p99_ns,container_bytes,allocated_bytes,physical_io_amplification,cache"
    );
    for (name, format) in [
        ("raw", ImageFormat::Raw),
        ("qcow2", ImageFormat::Qcow2),
        ("vdi", ImageFormat::Vdi),
        ("vmdk", ImageFormat::Vmdk),
        ("vhdx", ImageFormat::Vhdx),
    ] {
        if filter.as_ref().is_some_and(|filter| filter != name) {
            continue;
        }
        for sample in 1..=samples {
            let path = directory.path().join(format!("{name}-{sample}.img"));
            let mut expected = vec![0; size];
            let mut writer = ImageWriter::create_sparse(&path, format, size as u64)?;
            writer.flush()?;
            for (phase, partial, value) in [
                ("first_allocation", false, 0x37),
                ("partial_update", true, 0x61),
            ] {
                let start = Instant::now();
                let mut latency = requests(&writer, count, partial, value, &mut expected)?;
                writer.flush()?;
                report(
                    format,
                    phase,
                    sample,
                    (count * if partial { 512 } else { 4096 }) as u64,
                    start.elapsed().as_secs_f64(),
                    &mut latency,
                    &path,
                )?;
                verify_writer(&writer, &expected)?;
            }
            if format == ImageFormat::Qcow2 && cfg!(target_os = "linux") {
                writer.create_snapshot(b"lifecycle", b"baseline")?;
                let start = Instant::now();
                let mut latency = requests(&writer, count, true, 0x92, &mut expected)?;
                writer.flush()?;
                report(
                    format,
                    "snapshot_cow",
                    sample,
                    (count * 512) as u64,
                    start.elapsed().as_secs_f64(),
                    &mut latency,
                    &path,
                )?;
                verify_writer(&writer, &expected)?;
                writer.delete_snapshot(b"lifecycle")?;
            } else {
                eprintln!(
                    "excluded {format:?} snapshot_cow: requires supported Linux QCOW2 internal snapshots"
                );
            }
            let start = Instant::now();
            match writer.discard(0, 4096, DiscardPolicy::AllowZeroFallback) {
                Ok(_) => {
                    expected[..4096].fill(0);
                    writer.flush()?;
                    report(
                        format,
                        "discard_plus_flush",
                        sample,
                        4096,
                        start.elapsed().as_secs_f64(),
                        &mut [],
                        &path,
                    )?;
                    verify_writer(&writer, &expected)?;
                }
                Err(error) if error.kind() == virtdisk::io::ErrorKind::Unsupported => {
                    eprintln!("excluded {format:?} discard: {error}")
                }
                Err(error) => return Err(error.into()),
            }
            let start = Instant::now();
            match writer.resize((size + (1 << 20)) as u64, ShrinkPolicy::Reject) {
                Ok(()) => {
                    writer.flush()?;
                    report(
                        format,
                        "grow_plus_flush",
                        sample,
                        1 << 20,
                        start.elapsed().as_secs_f64(),
                        &mut [],
                        &path,
                    )?;
                    expected.resize(size + (1 << 20), 0);
                    verify_writer(&writer, &expected)?;
                    let start = Instant::now();
                    writer.resize(size as u64, ShrinkPolicy::RequireZero)?;
                    writer.flush()?;
                    report(
                        format,
                        "shrink_zero_tail_plus_flush",
                        sample,
                        1 << 20,
                        start.elapsed().as_secs_f64(),
                        &mut [],
                        &path,
                    )?;
                    expected.truncate(size);
                    verify_writer(&writer, &expected)?;
                }
                Err(error) if error.kind() == virtdisk::io::ErrorKind::Unsupported => {
                    eprintln!("excluded {format:?} grow: {error}")
                }
                Err(error) => return Err(error.into()),
            }
            drop(writer);
            verify(&path, format, &expected)?;
            let start = Instant::now();
            let writer = ImageWriter::open(&path, format)?;
            report(
                format,
                "clean_reopen",
                sample,
                0,
                start.elapsed().as_secs_f64(),
                &mut [],
                &path,
            )?;
            drop(writer);
            let start = Instant::now();
            let writer = ImageWriter::open_with_options(
                &path,
                format,
                &WriterOpenOptions::default().recovery_policy(RecoveryPolicy::Recover),
            )?;
            report(
                format,
                "clean_recovery_policy_open",
                sample,
                0,
                start.elapsed().as_secs_f64(),
                &mut [],
                &path,
            )?;
            drop(writer);
            verify(&path, format, &expected)?;
            let source = Image::open(&path, Some(format))?;
            let output = directory
                .path()
                .join(format!("compact-{name}-{sample}.img"));
            let start = Instant::now();
            compact_image(&source, &output, format)?;
            report(
                format,
                "compact_verified_publish",
                sample,
                expected.len() as u64,
                start.elapsed().as_secs_f64(),
                &mut [],
                &output,
            )?;
            drop(source);
            verify(&output, format, &expected)?;
            std::fs::remove_file(output)?;
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}
