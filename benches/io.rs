//! Stable, configurable Cargo I/O benchmark. All results are warm-cache workloads.
use std::{hint::black_box, io, path::Path, time::Instant};
use virtdisk::{Image, ImageFormat, ImageWriter, ReadAt, WriteAt};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Config {
    size: u64,
    block: usize,
    requests: usize,
    samples: usize,
    seed: u64,
    access: String,
    state: String,
    flush: usize,
    readers: usize,
    crossing: bool,
    write: bool,
    format: Option<String>,
}
impl Config {
    fn parse() -> Result<Self> {
        let mut config = Self {
            size: 64 << 20,
            block: 4096,
            requests: 16384,
            samples: 10,
            seed: 1,
            access: "stride".into(),
            state: "allocated".into(),
            flush: 0,
            readers: 1,
            crossing: false,
            write: false,
            format: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--smoke" => {
                    config.size = 4 << 20;
                    config.requests = 64;
                    config.samples = 1;
                }
                "--bench" => {} // Cargo supplies this argument.
                "--crossing" => config.crossing = true,
                "--write" => config.write = true,
                "--size" => config.size = args.next().ok_or("missing size")?.parse()?,
                "--block" => config.block = args.next().ok_or("missing block")?.parse()?,
                "--requests" => config.requests = args.next().ok_or("missing requests")?.parse()?,
                "--samples" => config.samples = args.next().ok_or("missing samples")?.parse()?,
                "--seed" => config.seed = args.next().ok_or("missing seed")?.parse()?,
                "--flush" => config.flush = args.next().ok_or("missing flush")?.parse()?,
                "--readers" => config.readers = args.next().ok_or("missing readers")?.parse()?,
                "--access" => config.access = args.next().ok_or("missing access")?,
                "--state" => config.state = args.next().ok_or("missing state")?,
                "--format" => config.format = Some(args.next().ok_or("missing format")?),
                _ => return Err(format!("unknown argument {arg}").into()),
            }
        }
        if config.size < config.block as u64 + 512
            || !config.size.is_multiple_of(512)
            || config.block == 0
            || !config.block.is_multiple_of(512)
            || config.block > 4 << 20
            || config.requests == 0
            || config.samples == 0
            || ![1, 2, 4, 8].contains(&config.readers)
            || config.write && config.readers != 1
        {
            return Err(
                "invalid size, request count, samples, or reader/writer concurrency".into(),
            );
        }
        if !["sequential", "stride", "shuffle", "uniform", "hot"].contains(&config.access.as_str())
            || !["allocated", "fresh", "zero", "fragmented", "cow"].contains(&config.state.as_str())
            || config.write && config.state == "zero"
        {
            return Err("invalid access/allocation combination".into());
        }
        if let Some(format) = &config.format
            && !["raw", "qcow2", "vdi", "vmdk", "vhdx"].contains(&format.as_str())
        {
            return Err("unknown format".into());
        }
        Ok(config)
    }
}

fn random(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9e3779b97f4a7c15);
    let mut value = *seed;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}
fn offsets(config: &Config) -> Vec<u64> {
    let shift = if config.crossing {
        (config.block / 2) as u64
    } else {
        0
    };
    let count = (config.size - shift) / config.block as u64;
    let mut seed = config.seed;
    // Bound permutation memory by the explicitly requested workload, even for huge sparse images.
    let mut permutation: Vec<_> = if config.access == "shuffle" {
        (0..count.min(config.requests as u64)).collect()
    } else {
        Vec::new()
    };
    if config.access == "shuffle" {
        for i in (1..permutation.len()).rev() {
            let j = random(&mut seed) as usize % (i + 1);
            permutation.swap(i, j);
        }
    }
    // Pick a stride coprime to the actual block count, including non-power-of-two capacities.
    let mut stride = 104729;
    while gcd(stride, count) != 1 {
        stride += 2;
    }
    (0..config.requests)
        .map(|i| {
            let index = match config.access.as_str() {
                "stride" => (i as u64).wrapping_mul(stride) % count,
                "shuffle" => permutation[i % permutation.len()],
                "uniform" => random(&mut seed) % count,
                "hot" => random(&mut seed) % (count / 16).max(1),
                _ => i as u64 % count,
            };
            index * config.block as u64 + shift
        })
        .collect()
}
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn report(
    config: &Config,
    format: ImageFormat,
    operation: &str,
    sample: usize,
    bytes: u64,
    seconds: f64,
    latencies: &mut [u128],
) {
    latencies.sort_unstable();
    let percentile = |percent: usize| {
        if latencies.len() >= 1000 {
            latencies[(latencies.len() - 1) * percent / 100].to_string()
        } else {
            "unavailable".into()
        }
    };
    println!(
        "{format:?},{operation},{},{sample},{bytes},{seconds:.9},{:.3},{:.3},{},{},{},{},{},{},{},{},{},{},{},warm",
        config.block,
        bytes as f64 / seconds / 1048576.0,
        if bytes == 0 {
            0.0
        } else {
            bytes as f64 / config.block as f64 / seconds
        },
        config.size,
        config.seed,
        config.access,
        config.state,
        config.flush,
        config.readers,
        config.crossing,
        latencies.len(),
        percentile(50),
        percentile(95),
        percentile(99)
    );
}

fn setup(path: &Path, config: &Config, format: ImageFormat) -> Result<u8> {
    let sparse = ["fresh", "zero", "fragmented"].contains(&config.state.as_str());
    let mut writer = if sparse {
        ImageWriter::create_sparse(path, format, config.size)?
    } else {
        ImageWriter::create(path, format, config.size)?
    };
    let initial = if ["fresh", "zero"].contains(&config.state.as_str()) {
        0
    } else {
        0x5a
    };
    if initial != 0 {
        let buffer = vec![initial; 1 << 20];
        let count = config.size.div_ceil(buffer.len() as u64);
        // Alternate low and high logical blocks to disrupt physical mapping locality.
        for i in 0..count {
            let block = if config.state == "fragmented" {
                if i % 2 == 0 { i / 2 } else { count - 1 - i / 2 }
            } else {
                i
            };
            let offset = block * buffer.len() as u64;
            let len = (config.size - offset).min(buffer.len() as u64) as usize;
            writer.write_all_at(offset, &buffer[..len])?;
        }
    }
    writer.flush()?;
    if config.state == "cow" {
        writer.create_snapshot(b"bench", b"benchmark baseline")?;
    }
    Ok(initial)
}

fn verify(
    path: &Path,
    format: ImageFormat,
    config: &Config,
    offsets: &[u64],
    initial: u8,
    payload: u8,
) -> Result<()> {
    let reader = Image::open(path, Some(format))?;
    let mut ranges: Vec<_> = if config.write {
        offsets
            .iter()
            .map(|&start| (start, start + config.block as u64))
            .collect()
    } else {
        Vec::new()
    };
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }
    let mut cursor = 0;
    let mut buffer = vec![0; 1 << 20];
    for (start, end) in merged.into_iter().chain([(config.size, config.size)]) {
        for (limit, expected) in [(start, initial), (end, payload)] {
            while cursor < limit {
                let len = (limit - cursor).min(buffer.len() as u64) as usize;
                reader.read_exact_at(cursor, &mut buffer[..len])?;
                if buffer[..len].iter().any(|&byte| byte != expected) {
                    return Err(format!("payload verification failed at {cursor}").into());
                }
                cursor += len as u64;
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let config = Config::parse()?;
    let offsets = offsets(&config);
    let directory = tempfile::tempdir()?;
    let timer = Instant::now();
    for _ in 0..10000 {
        black_box(Instant::now().elapsed());
    }
    eprintln!(
        "cache=warm; timer_pair_ns={}; flush=0 means per pass; no host cache eviction",
        timer.elapsed().as_nanos() / 10000
    );
    println!(
        "format,operation,block_bytes,sample,bytes,seconds,mib_per_second,iops,image_bytes,seed,access,state,flush_every,readers,crossing,latency_samples,p50_ns,p95_ns,p99_ns,cache"
    );
    for (name, format) in [
        ("raw", ImageFormat::Raw),
        ("qcow2", ImageFormat::Qcow2),
        ("vdi", ImageFormat::Vdi),
        ("vmdk", ImageFormat::Vmdk),
        ("vhdx", ImageFormat::Vhdx),
    ] {
        if config.format.as_ref().is_some_and(|filter| filter != name) {
            continue;
        }
        if config.state == "cow" && (format != ImageFormat::Qcow2 || !cfg!(target_os = "linux")) {
            eprintln!(
                "excluded {format:?}: internal snapshot COW requires supported Linux QCOW2 profile"
            );
            continue;
        }
        for sample in 1..=config.samples {
            let path = directory.path().join(format!("{name}-{sample}.img"));
            let start = Instant::now();
            let initial = setup(&path, &config, format)?;
            report(
                &config,
                format,
                "setup",
                sample,
                0,
                start.elapsed().as_secs_f64(),
                &mut [],
            );
            let payload = (sample % 254 + 1) as u8;
            let mut latencies = Vec::with_capacity(config.requests * config.readers);
            if config.write {
                let start = Instant::now();
                let writer = ImageWriter::open(&path, format)?;
                report(
                    &config,
                    format,
                    "open_writer",
                    sample,
                    0,
                    start.elapsed().as_secs_f64(),
                    &mut [],
                );
                let buffer = vec![payload; config.block];
                let whole = Instant::now();
                let mut flush_seconds = 0.0;
                for (i, &offset) in offsets.iter().enumerate() {
                    let start = Instant::now();
                    writer.write_all_at(black_box(offset), black_box(&buffer))?;
                    if config.flush != 0 && (i + 1) % config.flush == 0 {
                        let flush = Instant::now();
                        writer.flush()?;
                        flush_seconds += flush.elapsed().as_secs_f64();
                    }
                    latencies.push(start.elapsed().as_nanos());
                }
                let flush = Instant::now();
                writer.flush()?;
                flush_seconds += flush.elapsed().as_secs_f64();
                let total = whole.elapsed().as_secs_f64();
                let bytes = config.requests as u64 * config.block as u64;
                report(
                    &config,
                    format,
                    "write_plus_flush",
                    sample,
                    bytes,
                    total,
                    &mut latencies,
                );
                report(
                    &config,
                    format,
                    "write_excluding_flush",
                    sample,
                    bytes,
                    total - flush_seconds,
                    &mut [],
                );
                report(&config, format, "flush", sample, 0, flush_seconds, &mut []);
                let close = Instant::now();
                drop(writer);
                report(
                    &config,
                    format,
                    "close_writer",
                    sample,
                    0,
                    close.elapsed().as_secs_f64(),
                    &mut [],
                );
            } else {
                let start = Instant::now();
                let readers: Vec<_> = (0..config.readers)
                    .map(|_| Image::open(&path, Some(format)))
                    .collect::<virtdisk::io::Result<_>>()?;
                report(
                    &config,
                    format,
                    "open_readers",
                    sample,
                    0,
                    start.elapsed().as_secs_f64(),
                    &mut [],
                );
                let barrier = std::sync::Barrier::new(config.readers);
                let start = Instant::now();
                std::thread::scope(|scope| -> Result<()> {
                    let handles: Vec<_> = readers
                        .iter()
                        .map(|reader| {
                            let offsets = &offsets;
                            let barrier = &barrier;
                            let config = &config;
                            scope.spawn(move || -> io::Result<Vec<u128>> {
                                let mut buffer = vec![0; config.block];
                                let mut latency = Vec::with_capacity(offsets.len());
                                barrier.wait();
                                for &offset in offsets {
                                    let start = Instant::now();
                                    reader
                                        .read_exact_at(black_box(offset), black_box(&mut buffer))?;
                                    latency.push(start.elapsed().as_nanos());
                                    black_box(&buffer);
                                }
                                Ok(latency)
                            })
                        })
                        .collect();
                    for handle in handles {
                        latencies.extend(handle.join().map_err(|_| "reader thread panicked")??);
                    }
                    Ok(())
                })?;
                report(
                    &config,
                    format,
                    "read",
                    sample,
                    config.requests as u64 * config.block as u64 * config.readers as u64,
                    start.elapsed().as_secs_f64(),
                    &mut latencies,
                );
                let start = Instant::now();
                drop(readers);
                report(
                    &config,
                    format,
                    "close_readers",
                    sample,
                    0,
                    start.elapsed().as_secs_f64(),
                    &mut [],
                );
            }
            let start = Instant::now();
            verify(&path, format, &config, &offsets, initial, payload)?;
            report(
                &config,
                format,
                "verify",
                sample,
                config.size,
                start.elapsed().as_secs_f64(),
                &mut [],
            );
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}
