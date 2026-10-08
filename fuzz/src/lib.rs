//! Read-only disk parsing with input, read, and validation work bounds.
mod vhdx_chain;
mod vmdk_split;
mod writable;
use std::sync::Arc;
use virtdisk::io;
use virtdisk::{ParserLimits, Qcow2, Qcow2Writer, RawWriter, ReadAt, Vdi, Vhdx, Vmdk};

struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let end = start
            .checked_add(output.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "fuzz range overflow"))?;
        let source = self
            .0
            .get(start..end)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "fuzz source exhausted"))?;
        output.copy_from_slice(source);
        Ok(())
    }
}

/// Exercise bounded QCOW2 headers, mapping, compressed reads, and partitions.
/// Embedded backing names are never authorized or opened by this harness.
pub fn qcow2(data: &[u8]) {
    if data.len() > 1 << 20 {
        return;
    }
    let source: Arc<dyn ReadAt> = Arc::new(Bytes(data.to_vec()));
    let limits = ParserLimits {
        work_items: 4096,
        metadata_bytes: 256 << 10,
        cache_bytes: 64 << 10,
        decompressed_bytes: 16 << 20,
        ..ParserLimits::default()
    };
    let Ok(image) = Qcow2::open_with_limits(source, limits) else {
        return;
    };
    let image = Arc::new(image);
    let mut checkpoints = 0;
    let _ = image.validate_active_mapping_with_cancel(|| {
        checkpoints += 1;
        checkpoints > 32
    });
    let mut output = [0u8; 4096];
    for start in [
        0,
        511,
        4095,
        image.len() / 2,
        image.len().saturating_sub(4096),
    ] {
        let count = image.len().saturating_sub(start).min(output.len() as u64) as usize;
        let _ = image.read_exact_at(start, &mut output[..count]);
    }
    if let Ok(snapshots) = image.list_snapshots() {
        for snapshot in snapshots.iter().take(2) {
            if let Ok(view) = image.open_snapshot(&snapshot.id) {
                for offset in [0, 511, view.len().saturating_sub(4096)] {
                    let count = view.len().saturating_sub(offset).min(output.len() as u64) as usize;
                    let _ = view.read_exact_at(offset, &mut output[..count]);
                }
            }
        }
    }
}

pub fn run(target: &str, data: &[u8]) -> Result<(), &'static str> {
    match target {
        "qcow2" => qcow2(data),
        "vhdx-chain" => vhdx_chain::run(data),
        "vdi" | "vmdk" | "vhdx" => container(target, data),
        "raw-write" => raw_write(data),
        "qcow2-write" => qcow2_write(data),
        "vmdk-write" if matches!(data.first(), Some(4..=7)) => {
            vmdk_split::run(data);
        }
        "vdi-write" | "vmdk-write" | "vhdx-write" => writable::native(target, data),
        _ => return Err("unknown fuzz target"),
    }
    Ok(())
}

pub const TARGETS: &[&str] = &[
    "qcow2",
    "vdi",
    "vmdk",
    "vhdx",
    "raw-write",
    "qcow2-write",
    "vdi-write",
    "vmdk-write",
    "vhdx-write",
    "vhdx-chain",
];

/// Compare bounded QCOW2 mutations with an independent byte model.
///
/// Input supplies operations only, never filenames. Each case uses at most 64
/// operations, 192 KiB virtual capacity, and harness-owned temporary files.
/// Cases cover private payloads and authorized immutable raw backing images;
/// at most four internal snapshots retain independent byte models. Successful
/// reopen checkpoints validate exact allocation ownership and saved views.
pub fn qcow2_write(data: &[u8]) {
    qcow2_write_model(data);
}

fn qcow2_write_model(data: &[u8]) -> usize {
    if data.len() > 385 {
        return 0;
    }
    let overlay = data.first().is_none_or(|byte| byte & 1 == 0);
    if overlay && !cfg!(target_os = "linux") {
        return 0;
    }
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("base.raw");
    let child = directory.path().join("child.qcow2");
    let base: Vec<u8> = (0..131072).map(|index| (index % 251) as u8).collect();
    std::fs::write(&parent, &base).unwrap();
    let mut model = if overlay {
        base.clone()
    } else {
        vec![0; 131072]
    };
    model.resize(196608, 0);
    let open = || {
        if overlay {
            Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent))
        } else {
            Qcow2Writer::open(&child)
        }
    };
    let mut writer = if overlay {
        virtdisk::create_qcow2_overlay(&child, &parent, "raw", model.len() as u64).unwrap();
        open().unwrap()
    } else {
        Qcow2Writer::create(&child, model.len() as u64).unwrap()
    };
    let mut saved: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for record in data
        .get(1..)
        .unwrap_or(&[])
        .as_chunks::<6>()
        .0
        .iter()
        .take(64)
    {
        let offset = u16::from_le_bytes([record[1], record[2]]) as usize * 4;
        let length = u16::from_le_bytes([record[3], record[4]]) as usize % 1025;
        let valid = offset
            .checked_add(length)
            .is_some_and(|end| end <= model.len());
        let operation = record[0] % 12;
        match operation {
            0 => {
                let bytes = vec![record[5]; length];
                let result = writer.write_all_at(offset as u64, &bytes);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[offset..offset + length].copy_from_slice(&bytes);
                }
            }
            1 => {
                let result = writer.write_zeroes(offset as u64, length as u64);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[offset..offset + length].fill(0);
                }
            }
            2 => {
                writer.flush().unwrap();
                drop(writer);
                if overlay {
                    assert_eq!(
                        Qcow2Writer::open(&child).err().unwrap().kind(),
                        io::ErrorKind::PermissionDenied
                    );
                }
                let authorities = if overlay {
                    vec![parent.clone()]
                } else {
                    vec![]
                };
                let disk = Arc::new(Qcow2::open_chain(&child, &authorities).unwrap());
                disk.validate_active_mapping().unwrap();
                verify_saved_views(&disk, &saved);
                drop(disk);
                writer = open().unwrap();
            }
            3 => {
                let mut bytes = vec![0; length];
                let result = writer.read_exact_at(offset as u64, &mut bytes);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    assert_eq!(bytes, model[offset..offset + length]);
                }
            }
            4 => {
                assert!(writer.write_all_at(u64::MAX, &[record[5]]).is_err());
                assert!(writer.write_zeroes(model.len() as u64 + 1, 0).is_err());
                assert!(writer.read_exact_at(u64::MAX, &mut [0]).is_err());
            }
            6 => {
                let start = record[1] as usize % 3 * 65536;
                let result = virtdisk::WriteAt::discard(
                    &writer,
                    start as u64,
                    65536,
                    virtdisk::DiscardPolicy::AllowZeroFallback,
                );
                let valid = start + 65536 <= model.len();
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[start..start + 65536].fill(0);
                }
            }
            8 | 9 => {
                let new_size = u16::from_le_bytes([record[1], record[2]]) as usize % 385 * 512;
                let policy = if operation == 8 {
                    virtdisk::ShrinkPolicy::AllowDataLoss
                } else if record[5] & 1 == 0 {
                    virtdisk::ShrinkPolicy::RequireZero
                } else {
                    virtdisk::ShrinkPolicy::Reject
                };
                let allowed = new_size == model.len()
                    || (cfg!(target_os = "linux")
                        && (new_size >= model.len()
                            || policy == virtdisk::ShrinkPolicy::AllowDataLoss
                            || (policy == virtdisk::ShrinkPolicy::RequireZero
                                && model[new_size..].iter().all(|b| *b == 0))));
                let before = if allowed {
                    None
                } else {
                    Some(std::fs::read(&child).unwrap())
                };
                let result = writer.resize(new_size as u64, policy);
                assert_eq!(result.is_ok(), allowed);
                if let Some(before) = before {
                    assert_eq!(std::fs::read(&child).unwrap(), before);
                }
                if allowed {
                    model.resize(new_size, 0);
                }
            }
            7 => {
                let result = virtdisk::WriteAt::discard(
                    &writer,
                    offset as u64,
                    length as u64,
                    virtdisk::DiscardPolicy::AllowZeroFallback,
                );
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[offset..offset + length].fill(0);
                }
            }
            5 => {
                let mut id = vec![b'0' + record[1] % 4];
                let mut name = vec![record[2]];
                match record[5] % 4 {
                    1 => id.clear(),
                    2 => id.resize(257, b'x'),
                    3 => name.resize(257, b'x'),
                    _ => {}
                }
                let before = std::fs::read(&child).unwrap();
                let duplicate = saved.iter().any(|(saved_id, _)| *saved_id == id);
                let result = writer.create_snapshot(&id, &name);
                if !cfg!(target_os = "linux") {
                    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
                    assert_eq!(std::fs::read(&child).unwrap(), before);
                } else if record[5] % 4 != 0 {
                    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
                    assert_eq!(std::fs::read(&child).unwrap(), before);
                } else if duplicate {
                    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
                    assert_eq!(std::fs::read(&child).unwrap(), before);
                } else {
                    let snapshot = result.unwrap();
                    assert_eq!(snapshot.id, id);
                    assert_eq!(snapshot.virtual_size, model.len() as u64);
                    saved.push((id.clone(), model.clone()));
                    let after = std::fs::read(&child).unwrap();
                    assert_eq!(
                        writer.create_snapshot(&id, &name).unwrap_err().kind(),
                        io::ErrorKind::AlreadyExists
                    );
                    assert_eq!(std::fs::read(&child).unwrap(), after);
                }
            }
            10 | 11 => {
                let id = vec![b'0' + record[1] % 4];
                let selected = saved.iter().position(|(saved_id, _)| *saved_id == id);
                let before = std::fs::read(&child).unwrap();
                let result = if operation == 10 {
                    writer.delete_snapshot(&id)
                } else {
                    writer.revert_snapshot(&id)
                };
                if !cfg!(target_os = "linux") {
                    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
                    assert_eq!(std::fs::read(&child).unwrap(), before);
                } else if let Some(index) = selected {
                    let snapshot = result.unwrap();
                    assert_eq!(snapshot.id, id);
                    assert_eq!(snapshot.virtual_size, saved[index].1.len() as u64);
                    if operation == 10 {
                        saved.remove(index);
                    } else {
                        model = saved[index].1.clone();
                    }
                } else {
                    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
                    assert_eq!(std::fs::read(&child).unwrap(), before);
                }
            }
            _ => {
                writer.flush().unwrap();
                drop(writer);
                let authorities = if overlay {
                    vec![parent.clone()]
                } else {
                    vec![]
                };
                let disk = Arc::new(Qcow2::open_chain(&child, &authorities).unwrap());
                disk.validate_active_mapping().unwrap();
                verify_saved_views(&disk, &saved);
                let mut actual = vec![0; model.len()];
                disk.read_exact_at(0, &mut actual).unwrap();
                assert_eq!(actual, model);
                drop(disk);
                writer = open().unwrap();
            }
        }
        assert_eq!(writer.len(), model.len() as u64);
        let mut actual = vec![0; model.len()];
        writer.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, model);
    }
    writer.flush().unwrap();
    drop(writer);
    let authorities = if overlay {
        vec![parent.clone()]
    } else {
        vec![]
    };
    let disk = Arc::new(Qcow2::open_chain(&child, &authorities).unwrap());
    disk.validate_active_mapping().unwrap();
    verify_saved_views(&disk, &saved);
    let mut actual = vec![0; model.len()];
    disk.read_exact_at(0, &mut actual).unwrap();
    assert_eq!(actual, model);
    assert_eq!(std::fs::read(parent).unwrap(), base);
    saved.len()
}

fn verify_saved_views(image: &Arc<Qcow2>, saved: &[(Vec<u8>, Vec<u8>)]) {
    assert_eq!(image.list_snapshots().unwrap().len(), saved.len());
    for (id, model) in saved {
        let view = image.open_snapshot(id).unwrap();
        assert_eq!(view.len(), model.len() as u64);
        let mut actual = vec![0; model.len()];
        view.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, *model);
    }
}

/// Compare bounded raw mutation sequences against an independent byte model.
pub fn raw_write(data: &[u8]) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("model.raw");
    let mut model = vec![0; 4096];
    let mut writer = RawWriter::create(&path, model.len() as u64).unwrap();
    for record in data.as_chunks::<6>().0.iter().take(128) {
        let offset = u16::from_le_bytes([record[1], record[2]]) as usize % 8193;
        let length = u16::from_le_bytes([record[3], record[4]]) as usize % 513;
        let valid = offset
            .checked_add(length)
            .is_some_and(|end| end <= model.len());
        match record[0] % 7 {
            0 => {
                let bytes = vec![record[5]; length];
                let result = writer.write_all_at(offset as u64, &bytes);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[offset..offset + length].copy_from_slice(&bytes);
                }
            }
            1 => {
                let result = writer.write_zeroes(offset as u64, length as u64);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[offset..offset + length].fill(0);
                }
            }
            2 => {
                writer.resize(offset as u64).unwrap();
                model.resize(offset, 0);
            }
            3 => {
                writer.flush().unwrap();
                drop(writer);
                writer = RawWriter::open(&path).unwrap();
            }
            6 => {
                let result = writer.preallocate(offset as u64, length as u64);
                if !valid {
                    assert!(result.is_err());
                } else if let Err(error) = result {
                    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
                }
            }
            5 => {
                let result = virtdisk::WriteAt::discard(
                    &writer,
                    offset as u64,
                    length as u64,
                    virtdisk::DiscardPolicy::AllowZeroFallback,
                );
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[offset..offset + length].fill(0);
                }
            }
            _ => {
                let mut bytes = vec![0; length];
                let result = writer.read_exact_at(offset as u64, &mut bytes);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    assert_eq!(bytes, model[offset..offset + length]);
                }
            }
        }
        assert_eq!(writer.len(), model.len() as u64);
        let mut actual = vec![0; model.len()];
        writer.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, model);
    }
}

/// Exercise new format readers with independently tightened metadata budgets.
pub fn container(target: &str, data: &[u8]) {
    if data.len() > 8 << 20 {
        return;
    }
    let limits = ParserLimits {
        metadata_bytes: 4 << 20,
        cache_bytes: 4 << 20,
        work_items: 16_384,
        ..Default::default()
    };
    let source: Arc<dyn ReadAt> = Arc::new(Bytes(data.to_vec()));
    let image: Box<dyn ReadAt> = match target {
        "vdi" => match Vdi::open_with_limits(source, limits) {
            Ok(image) => Box::new(image),
            Err(_) => return,
        },
        "vmdk" => match Vmdk::open_with_limits(source, limits) {
            Ok(image) => Box::new(image),
            Err(_) => return,
        },
        "vhdx" => match Vhdx::open_with_limits(source, limits) {
            Ok(image) => Box::new(image),
            Err(_) => return,
        },
        _ => return,
    };
    let mut output = [0; 4096];
    for offset in [
        0,
        511,
        4095,
        image.len() / 2,
        image.len().saturating_sub(4096),
        image.len(),
        u64::MAX,
    ] {
        let count = image.len().saturating_sub(offset).min(output.len() as u64) as usize;
        let _ = image.read_exact_at(offset, &mut output[..count]);
    }
}

fn qcow_lifecycle_seed(mode: u8) -> Vec<u8> {
    let mut data = vec![mode];
    for record in [
        [5, 0, 0, 0, 0, 0],
        [0, 0, 0, 16, 0, 42],
        [5, 1, 0, 0, 0, 0],
        [0, 0, 0, 16, 0, 17],
        [11, 0, 0, 0, 0, 0],
        [2, 0, 0, 0, 0, 0],
        [10, 0, 0, 0, 0, 0],
        [0, 0, 0, 16, 0, 27],
        [11, 1, 0, 0, 0, 0],
        [10, 1, 0, 0, 0, 0],
        [10, 1, 0, 0, 0, 0],
        [11, 1, 0, 0, 0, 0],
        [5, 0, 0, 0, 0, 0],
        [10, 0, 0, 0, 0, 0],
        [8, 3, 0, 0, 0, 0],
        [2, 0, 0, 0, 0, 0],
    ] {
        data.extend(record);
    }
    data
}

pub fn seeds(target: &str) -> Vec<Vec<u8>> {
    if target == "vhdx-chain" {
        return vhdx_chain::seeds();
    }
    if ["vdi-write", "vmdk-write", "vhdx-write"].contains(&target) {
        let mut seeds = writable::seeds(target);
        if target == "vmdk-write" {
            seeds.extend(vmdk_split::seeds());
        }
        return seeds;
    }
    if target == "qcow2-write" {
        return vec![
            // Preserve the original seed order for deterministic replay.
            vec![
                1, 0, 0, 0, 16, 0, 31, 5, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0, 47, 5, 1, 0, 0, 0, 0, 6,
                0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 8, 1, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 5, 2, 0, 0, 0,
                1, 5, 2, 0, 0, 0, 2, 5, 2, 0, 0, 0, 3,
            ],
            vec![0, 5, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 1, 2, 0, 0, 0, 0, 0],
            vec![
                1, 9, 1, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 8, 128, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0,
            ],
            vec![
                1, 0, 0, 0, 16, 0, 9, 8, 1, 0, 0, 0, 0, 8, 128, 1, 0, 0, 0, 9, 0, 0, 0, 0, 0, 2, 0,
                0, 0, 0, 0,
            ],
            vec![0, 8, 1, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0],
            vec![0, 6, 0, 0, 0, 0, 0, 7, 1, 0, 15, 0, 0, 2, 0, 0, 0, 0, 0],
            vec![],
            vec![
                0, 0, 255, 63, 8, 0, 7, 1, 0, 0, 17, 0, 0, 2, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0,
            ],
            vec![
                1, 0, 255, 63, 8, 0, 7, 1, 0, 128, 17, 0, 0, 3, 255, 63, 8, 0, 0, 4, 0, 0, 0, 0, 0,
                5, 0, 0, 0, 0, 0,
            ],
            qcow_lifecycle_seed(1),
            qcow_lifecycle_seed(0),
        ];
    }
    if target == "raw-write" {
        return vec![
            vec![
                0, 0, 0, 16, 0, 9, 6, 0, 0, 16, 0, 0, 6, 255, 255, 16, 0, 0, 3, 0, 0, 0, 0, 0,
            ],
            vec![0, 0, 0, 16, 0, 9, 5, 1, 0, 8, 0, 0, 3, 0, 0, 0, 0, 0],
            vec![],
            vec![
                0, 0, 0, 16, 0, 17, 1, 4, 0, 4, 0, 0, 3, 0, 0, 0, 0, 0, 2, 8, 0, 0, 0, 0,
            ],
        ];
    }
    if target == "vhdx" {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("seed.vhdx");
        virtdisk::create_vhdx(&path, &Bytes(vec![42; 512])).unwrap();
        let image = std::fs::read(path).unwrap();
        assert!(Vhdx::open(Arc::new(Bytes(image.clone()))).is_ok());
        return vec![image, vec![0; 512], vec![]];
    }
    if target == "vdi" {
        let mut image = vec![0; 1536];
        for (offset, value) in [
            (64, 0xbeda107fu32),
            (68, 0x10001),
            (72, 400),
            (76, 1),
            (340, 512),
            (344, 1024),
            (360, 512),
            (376, 512),
            (384, 2),
            (388, 1),
            (516, u32::MAX),
        ] {
            image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        image[368..376].copy_from_slice(&1024u64.to_le_bytes());
        image[392..408].fill(11);
        image[408..424].fill(12);
        image[1024..].fill(17);
        assert!(Vdi::open(Arc::new(Bytes(image.clone()))).is_ok());
        return vec![image, vec![0; 512], vec![]];
    }
    if target == "vmdk" {
        let mut image = vec![0; 2048];
        image[..4].copy_from_slice(b"KDMV");
        for (offset, value) in [(4, 1u32), (44, 2), (512, 2), (1024, 3)] {
            image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [(12, 2u64), (20, 1), (56, 1), (64, 3)] {
            image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        image[1536..].fill(42);
        assert!(Vmdk::open(Arc::new(Bytes(image.clone()))).is_ok());
        return vec![image, vec![0; 512], vec![]];
    }
    let mut image = vec![0; 4096];
    image[..4].copy_from_slice(b"QFI\xfb");
    for (offset, value) in [(4, 3u32), (20, 9), (36, 1), (56, 1), (96, 4), (100, 104)] {
        image[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (offset, value) in [
        (24, 1536u64),
        (40, 512),
        (48, 1024),
        (512, 1536),
        (1536, 2048),
    ] {
        image[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    image[2048..2560].fill(0x5a);
    let accepted: Arc<dyn ReadAt> = Arc::new(Bytes(image.clone()));
    assert!(
        Qcow2::open(accepted).is_ok(),
        "valid QCOW2 seed must reach mapped reader"
    );
    vec![
        image,
        qcow_snapshot_seed(),
        qcow_shared_l1_snapshot_seed(),
        vec![0; 512],
        vec![],
    ]
}

fn qcow_snapshot_seed() -> Vec<u8> {
    let mut image = vec![0; 4096];
    image[..4].copy_from_slice(b"QFI\xfb");
    for (offset, value) in [
        (4, 3u32),
        (20, 9),
        (36, 1),
        (56, 1),
        (60, 1),
        (96, 4),
        (100, 104),
        (3584 + 8, 1),
        (3584 + 16, 42),
        (3584 + 20, 17),
        (3584 + 36, 16),
    ] {
        image[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (offset, value) in [
        (24, 512u64),
        (40, 1536),
        (48, 512),
        (64, 3584),
        (512, 1024),
        (1536, 2048),
        (2048, 2560),
        (3072, 2048),
        (3584, 3072),
        (3584 + 48, 512),
    ] {
        image[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    for (index, count) in [1u16, 1, 1, 1, 2, 2, 1, 1].iter().enumerate() {
        image[1024 + index * 2..1026 + index * 2].copy_from_slice(&count.to_be_bytes());
    }
    image[3584 + 12..3584 + 14].copy_from_slice(&1u16.to_be_bytes());
    image[3584 + 14..3584 + 16].copy_from_slice(&5u16.to_be_bytes());
    image[3584 + 56] = b'1';
    image[3584 + 57..3584 + 62].copy_from_slice(b"state");
    image[2560..3072].fill(42);
    image
}

fn qcow_shared_l1_snapshot_seed() -> Vec<u8> {
    let mut image = qcow_snapshot_seed();
    image[3584..3592].copy_from_slice(&1536u64.to_be_bytes());
    image[1030..1032].copy_from_slice(&2u16.to_be_bytes());
    image[1036..1038].copy_from_slice(&0u16.to_be_bytes());
    image
}

#[cfg(test)]
mod smoke {
    #[test]
    fn shared_l1_disk_snapshot_seed_remains_readable() {
        use super::*;
        let image = Arc::new(Qcow2::open(Arc::new(Bytes(qcow_shared_l1_snapshot_seed()))).unwrap());
        image.validate_active_mapping().unwrap();
        let view = image.open_snapshot(b"1").unwrap();
        let mut bytes = [0; 512];
        view.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [42; 512]);
    }
    #[test]
    fn qcow_snapshot_seed_has_valid_ownership_and_saved_bytes() {
        use super::*;
        let seed = seeds("qcow2")
            .into_iter()
            .find(|seed| seed.len() >= 64 && seed[60..64] == 1u32.to_be_bytes())
            .expect("disk snapshot seed required");
        let image = Arc::new(Qcow2::open(Arc::new(Bytes(seed))).unwrap());
        image.validate_active_mapping().unwrap();
        let snapshots = image.list_snapshots().unwrap();
        let view = image.open_snapshot(&snapshots[0].id).unwrap();
        let mut bytes = [0; 512];
        view.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [42; 512]);
    }
    #[test]
    fn corpus_and_truncations_exercise_owned_harnesses() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone, Copy, Debug)]
        enum Variant {
            Full,
            Prefix(usize),
            Mutation(usize),
        }

        let corpus: Vec<_> = super::TARGETS
            .iter()
            .map(|&target| (target, super::seeds(target)))
            .collect();
        let mut cases = Vec::new();
        for (target, seeds) in &corpus {
            for (index, seed) in seeds.iter().enumerate() {
                cases.push((*target, index, seed, Variant::Full));
                for end in [0, seed.len() / 2, seed.len().saturating_sub(1)] {
                    cases.push((*target, index, seed, Variant::Prefix(end)));
                }
                for offset in (0..seed.len()).step_by((seed.len() / 16).max(1)) {
                    cases.push((*target, index, seed, Variant::Mutation(offset)));
                }
            }
        }

        // Each replay owns its files. Bound concurrent image buffers and durability
        // work while balancing short parser cases against expensive writer cases.
        let workers = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(4);
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    while let Some(&(target, index, seed, variant)) =
                        cases.get(next.fetch_add(1, Ordering::Relaxed))
                    {
                        let result = std::panic::catch_unwind(|| match variant {
                            Variant::Full => super::run(target, seed).unwrap(),
                            Variant::Prefix(end) => super::run(target, &seed[..end]).unwrap(),
                            Variant::Mutation(offset) => {
                                let mut mutation = seed.clone();
                                mutation[offset] ^= 0xff;
                                super::run(target, &mutation).unwrap();
                            }
                        });
                        if let Err(error) = result {
                            eprintln!("failed replay: {target}, seed {index}, {variant:?}");
                            std::panic::resume_unwind(error);
                        }
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod format_targets {
    #[test]
    fn vdi_and_vmdk_targets_are_registered_and_replayable() {
        for target in ["vdi", "vmdk", "vhdx", "raw-write", "qcow2-write"] {
            assert!(super::TARGETS.contains(&target));
            super::run(target, &[]).unwrap();
        }
    }
}

#[cfg(test)]
mod writable_qcow2_target {
    #[test]
    #[cfg(target_os = "linux")]
    fn snapshot_lifecycle_sequences_revert_delete_and_reuse_ids() {
        let mut data = vec![1];
        for record in [
            [5, 0, 0, 0, 0, 0],
            [0, 0, 0, 16, 0, 42],
            [5, 1, 0, 0, 0, 0],
            [0, 0, 0, 16, 0, 17],
            [11, 0, 0, 0, 0, 0],
            [2, 0, 0, 0, 0, 0],
            [10, 0, 0, 0, 0, 0],
            [0, 0, 0, 16, 0, 27],
            [11, 1, 0, 0, 0, 0],
            [10, 1, 0, 0, 0, 0],
            [10, 1, 0, 0, 0, 0],
            [11, 1, 0, 0, 0, 0],
            [5, 0, 0, 0, 0, 0],
            [10, 0, 0, 0, 0, 0],
            [8, 3, 0, 0, 0, 0],
            [2, 0, 0, 0, 0, 0],
        ] {
            data.extend(record);
        }
        assert_eq!(super::qcow2_write_model(&data), 0);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn snapshot_sequence_preserves_saved_models_after_mutation_and_reopen() {
        let sequence = [
            1, 0, 0, 0, 16, 0, 31, 5, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0, 47, 5, 1, 0, 0, 0, 0, 6, 0, 0,
            0, 0, 0, 2, 0, 0, 0, 0, 0,
        ];
        assert_eq!(super::qcow2_write_model(&sequence), 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn snapshot_seed_covers_duplicate_negative_metadata_and_authorized_backing() {
        let seeds = super::seeds("qcow2-write");
        assert_eq!(super::qcow2_write_model(&seeds[0]), 2);
        assert_eq!(super::qcow2_write_model(&seeds[1]), 1);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn backed_snapshot_retains_parent_bytes_across_resize_reopen_and_revert() {
        let mut data = vec![0];
        for record in [
            [5, 0, 0, 0, 0, 0],   // Save inherited parent bytes at the original capacity.
            [8, 1, 0, 0, 0, 0],   // Shrink to 512 bytes.
            [8, 128, 1, 0, 0, 0], // Grow to 196608 bytes with a zero suffix.
            [2, 0, 0, 0, 0, 0],   // Reopen with explicit parent authority and verify saved bytes.
            [11, 0, 0, 0, 0, 0],  // Restore the snapshot, including inherited parent bytes.
            [2, 0, 0, 0, 0, 0],
            [10, 0, 0, 0, 0, 0],
        ] {
            data.extend(record);
        }
        assert_eq!(super::qcow2_write_model(&data), 0);
    }

    #[test]
    fn deterministic_qcow2_write_sequences_match_byte_model() {
        for mode in [0u8, 1] {
            let mut data = vec![mode];
            let mut state = 0x932e_127a_5569_u64;
            for _ in 0..64 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                data.extend_from_slice(&state.to_le_bytes()[..6]);
            }
            super::run("qcow2-write", &data).unwrap();
            data.push(0); // Explicit input-size ceiling: no image operations.
            super::run("qcow2-write", &data).unwrap();
        }
    }

    #[test]
    fn qcow2_writer_target_is_registered_seeded_and_replayable() {
        assert!(super::TARGETS.contains(&"qcow2-write"));
        for seed in super::seeds("qcow2-write") {
            super::run("qcow2-write", &seed).unwrap();
        }
    }
}

#[cfg(test)]
mod writable_native_targets {
    #[test]
    fn native_writers_are_registered_and_match_replayed_models() {
        for target in ["vdi-write", "vmdk-write", "vhdx-write"] {
            assert!(super::TARGETS.contains(&target));
            for seed in super::seeds(target) {
                super::run(target, &seed).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod differencing_bitmap_target {
    #[test]
    fn bitmap_chain_is_registered_and_valid_seed_reaches_cross_sector_reads() {
        assert!(super::TARGETS.contains(&"vhdx-chain"));
        let seeds = super::seeds("vhdx-chain");
        assert!(!seeds.is_empty());
        super::run("vhdx-chain", &seeds[0]).unwrap();
    }
}

#[cfg(test)]
mod capacity_preallocation_corpus {
    #[test]
    fn new_management_operations_have_explicit_replay_seeds() {
        assert!(super::seeds("qcow2-write").iter().any(|seed| {
            seed.get(1..)
                .unwrap_or(&[])
                .as_chunks::<6>()
                .0
                .iter()
                .any(|record| record[0] == 8)
        }));
        assert!(
            super::seeds("raw-write").iter().any(|seed| seed
                .as_chunks::<6>()
                .0
                .iter()
                .any(|record| record[0] == 6))
        );
        assert!(
            super::seeds("vdi-write")
                .iter()
                .any(|seed| seed.first().is_some_and(|mode| mode & 2 != 0))
        );
    }
}

#[cfg(test)]
mod native_management_corpus {
    #[test]
    #[cfg(target_os = "linux")]
    fn vmdk_and_vhdx_capacity_sequences_update_the_byte_model() {
        let mut data = vec![1];
        for record in [
            [0, 0, 0, 16, 0, 42],
            [8, 3, 0, 0, 0, 0],
            [8, 4, 0, 0, 0, 0],
            [2, 0, 0, 0, 0, 0],
        ] {
            data.extend(record);
        }
        for target in ["vmdk-write", "vhdx-write"] {
            assert_eq!(super::writable::native_model(target, &data), 2048);
        }
    }
    #[test]
    fn vdi_capacity_and_vhdx_final_discard_cases_are_seeded_and_replayable() {
        assert!(super::seeds("vdi-write").iter().any(|seed| {
            seed.get(1..)
                .unwrap_or(&[])
                .as_chunks::<6>()
                .0
                .iter()
                .any(|record| record[0] == 8)
        }));
        assert!(
            super::seeds("vhdx-write")
                .iter()
                .any(|seed| seed.first().is_some_and(|mode| mode & 2 != 0))
        );
        for target in ["vdi-write", "vhdx-write"] {
            for seed in super::seeds(target) {
                super::run(target, &seed).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod qcow_disk_snapshot_corpus {
    use super::*;
    #[test]
    fn positive_seed_reconstructs_shared_ownership_and_saved_disk_bytes() {
        let seed = seeds("qcow2")
            .into_iter()
            .find(|seed| seed.get(60..64) == Some(1u32.to_be_bytes().as_slice()))
            .expect("a valid internal snapshot seed is required");
        let disk = Arc::new(
            Qcow2::open_with_limits(
                Arc::new(Bytes(seed)),
                ParserLimits {
                    work_items: 4096,
                    ..ParserLimits::default()
                },
            )
            .unwrap(),
        );
        disk.validate_active_mapping().unwrap();
        let snapshots = disk.list_snapshots().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, b"1");
        assert_eq!(snapshots[0].name, b"state");
        assert_eq!(snapshots[0].virtual_size, 512);
        let view = disk.open_snapshot(b"1").unwrap();
        let mut bytes = [0; 512];
        view.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [42; 512]);
    }
}
