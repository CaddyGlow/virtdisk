use sha2::{Digest, Sha256};
use std::{io, path::Path};
use virtdisk::{DiscardPolicy, ReadAt, Vhdx, VhdxWriter};
use virtdisk_windows_acceptance::*;
fn emit(
    root: &Path,
    image: &str,
    parents: Vec<String>,
    sector: u32,
    mode: Mode,
    expected: &[u8],
) -> io::Result<()> {
    let paths: Vec<_> = parents.iter().map(|p| root.join(p)).collect();
    let view = if mode == Mode::Clean {
        Vhdx::open_chain(root.join(image), &paths)?
    } else {
        Vhdx::open_recovered_chain(root.join(image), &paths)?
    };
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    let mut offset = 0;
    while offset < view.len() {
        let n = (view.len() - offset).min(buffer.len() as u64) as usize;
        view.read_exact_at(offset, &mut buffer[..n])?;
        hash.update(&buffer[..n]);
        offset += n as u64;
    }
    if format!("{:x}", hash.finalize()) != format!("{:x}", Sha256::digest(expected)) {
        return Err(io::Error::other(
            "prepared image disagrees with independent model",
        ));
    }
    let mut files = Vec::new();
    for path in std::iter::once(image.to_owned()).chain(parents.iter().cloned()) {
        let full = root.join(&path);
        files.push(FileIdentity {
            path,
            length: std::fs::metadata(&full)?.len(),
            sha256: hash_file(&full)?,
        });
    }
    let m = Manifest {
        version: 1,
        image: image.into(),
        parents,
        files,
        capacity: view.len(),
        logical_sector: sector,
        physical_sector: 4096,
        logical_sha256: format!("{:x}", Sha256::digest(expected)),
        mode,
    };
    validate(root, &m)?;
    strict_locators(root, &m)?;
    let out = root.join(format!("{image}.json"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut file, &m)?;
    file.sync_all()
}
// Reconstruct the actual writer's durable-redo-before-metadata boundary.
// Payload and complete native log remain from the successful transaction;
// metadata targets revert to the exact pretransaction sectors.
fn reconstruct_redo_cut(path: &Path, before: &[u8]) -> io::Result<()> {
    let mut bytes = std::fs::read(path)?;
    let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    let log = u64_at(&bytes, 65536 + 72) as usize;
    let slot = [log, log + 16384]
        .into_iter()
        .filter(|&at| bytes.get(at..at + 4) == Some(b"loge"))
        .max_by_key(|&at| u64_at(&bytes, at + 16))
        .ok_or_else(|| io::Error::other("missing retained native redo"))?;
    let guid: [u8; 16] = bytes[slot + 32..slot + 48].try_into().unwrap();
    let count = u32::from_le_bytes(bytes[slot + 24..slot + 28].try_into().unwrap()) as usize;
    if count == 0 || count > 4 {
        return Err(io::Error::other("unexpected bounded descriptor count"));
    }
    for index in 0..count {
        let descriptor = slot + 64 + 32 * index;
        let target = u64_at(&bytes, descriptor + 16) as usize;
        let length = match &bytes[descriptor..descriptor + 4] {
            b"desc" => 4096,
            b"zero" => u64_at(&bytes, descriptor + 8) as usize,
            _ => return Err(io::Error::other("unknown redo descriptor")),
        };
        let end = target
            .checked_add(length)
            .ok_or_else(|| io::Error::other("redo overflow"))?;
        let original = before
            .get(target..end)
            .ok_or_else(|| io::Error::other("redo target outside old file"))?;
        bytes[target..end].copy_from_slice(original);
    }
    for header in [65536, 131072] {
        bytes[header + 48..header + 64].copy_from_slice(&guid);
        bytes[header + 4..header + 8].fill(0);
        let mut crc = !0u32;
        for &byte in &bytes[header..header + 4096] {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 != 0 { 0x82f63b78 } else { 0 };
            }
        }
        bytes[header + 4..header + 8].copy_from_slice(&(!crc).to_le_bytes());
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()
}
fn prepare(root: &Path) -> io::Result<()> {
    // Never overwrite or adopt an existing fixture directory.
    std::fs::create_dir(root)?;
    for sector in [512u32, 4096] {
        let directory = root.join(format!("sector-{sector}"));
        std::fs::create_dir(&directory)?;
        let parent = directory.join("base.vhdx");
        let child = directory.join("child.vhdx");
        let base = VhdxWriter::create(&parent, 4 << 20)?;
        base.write_all_at(0, &vec![7; 4 << 20])?;
        base.flush()?;
        drop(base);
        if sector == 4096 {
            // Native metadata fixture geometry, before creating any children.
            use std::io::{Read, Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&parent)?;
            let mut offset = [0; 8];
            file.seek(SeekFrom::Start(196672))?;
            file.read_exact(&mut offset)?;
            file.seek(SeekFrom::Start(u64::from_le_bytes(offset) + 65584))?;
            file.write_all(&sector.to_le_bytes())?;
            file.sync_all()?;
        }
        let writer = VhdxWriter::create_overlay(&child, &parent, &[])?;
        writer.write_all_at(u64::from(sector) - 3, &[9; 8])?;
        writer.write_all_at((1 << 20) + 17, &[8; 3])?;
        writer.discard(2 << 20, 1 << 20, DiscardPolicy::RequireDeallocation)?;
        writer.write_all_at((2 << 20) + 3, &[6])?;
        writer.write_zeroes((3 << 20) + 1, 5)?;
        writer.flush()?;
        drop(writer);
        let base_model = vec![7; 4 << 20];
        let mut child_model = base_model.clone();
        child_model[sector as usize - 3..sector as usize + 5].fill(9);
        child_model[(1 << 20) + 17..(1 << 20) + 20].fill(8);
        child_model[2 << 20..3 << 20].fill(0);
        child_model[(2 << 20) + 3] = 6;
        child_model[(3 << 20) + 1..(3 << 20) + 6].fill(0);
        emit(
            &directory,
            "base.vhdx",
            vec![],
            sector,
            Mode::Clean,
            &base_model,
        )?;
        emit(
            &directory,
            "child.vhdx",
            vec!["base.vhdx".into()],
            sector,
            Mode::Clean,
            &child_model,
        )?;
        for (mode_name, mode) in [
            ("native_replay", Mode::NativeReplay),
            ("library_recovery", Mode::LibraryRecovery),
        ] {
            let mode_root = directory.join(mode_name);
            std::fs::create_dir(&mode_root)?;
            for (image, model) in [("base.vhdx", &base_model), ("child.vhdx", &child_model)] {
                let fixture = mode_root.join(image);
                std::fs::create_dir(&fixture)?;
                std::fs::copy(directory.join(image), fixture.join(image))?;
                let parents = if image == "child.vhdx" {
                    std::fs::copy(&parent, fixture.join("base.vhdx"))?;
                    vec!["base.vhdx".into()]
                } else {
                    vec![]
                };
                let file = fixture.join(image);
                let before = std::fs::read(&file)?;
                let writer = VhdxWriter::open_chain(
                    &file,
                    &parents.iter().map(|p| fixture.join(p)).collect::<Vec<_>>(),
                )?;
                writer.write_all_at(8192, &[11; 8])?;
                writer.flush()?;
                drop(writer);
                reconstruct_redo_cut(&file, &before)?;
                let mut expected = model.clone();
                expected[8192..8200].fill(11);
                emit(&fixture, image, parents, sector, mode, &expected)?;
            }
        }
    }
    Ok(())
}
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        return Err(io::Error::other("usage: prepare NEW_FIXTURE_DIRECTORY"));
    }
    prepare(Path::new(&args[1]))
}
