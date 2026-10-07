//! Bounded native writer models over harness-owned files and parents.
use std::path::{Path, PathBuf};
use virtdisk::{VdiWriter, VhdxWriter, VmdkWriter, WriteAt};
const SIZE: usize = 2 * 1048576;
enum Writer {
    Vdi(VdiWriter),
    Vmdk(VmdkWriter),
    Vhdx(Box<VhdxWriter>),
}
impl Writer {
    fn create(target: &str, path: &Path, size: u64) -> Self {
        match target {
            "vdi-write" => Self::Vdi(VdiWriter::create_sparse(path, size).unwrap()),
            "vmdk-write" => Self::Vmdk(VmdkWriter::create_sparse(path, size).unwrap()),
            _ => Self::Vhdx(Box::new(VhdxWriter::create(path, size).unwrap())),
        }
    }
    fn overlay(target: &str, path: &Path, parent: &Path) -> Self {
        match target {
            "vdi-write" => Self::Vdi(VdiWriter::create_overlay(path, parent, &[]).unwrap()),
            "vmdk-write" => Self::Vmdk(
                VmdkWriter::create_overlay(path, parent, &[parent.to_path_buf()]).unwrap(),
            ),
            _ => Self::Vhdx(Box::new(
                VhdxWriter::create_overlay(path, parent, &[]).unwrap(),
            )),
        }
    }
    fn open(target: &str, path: &Path, parents: &[PathBuf]) -> Self {
        match target {
            "vdi-write" => Self::Vdi(VdiWriter::open_chain(path, parents).unwrap()),
            "vmdk-write" => Self::Vmdk(VmdkWriter::open_chain(path, parents).unwrap()),
            _ => Self::Vhdx(Box::new(VhdxWriter::open_chain(path, parents).unwrap())),
        }
    }
    fn writer(&self) -> &dyn WriteAt {
        match self {
            Self::Vdi(w) => w,
            Self::Vmdk(w) => w,
            Self::Vhdx(w) => w.as_ref(),
        }
    }
    fn read(&self, offset: u64, dst: &mut [u8]) -> std::io::Result<()> {
        match self {
            Self::Vdi(w) => w.read_exact_at(offset, dst),
            Self::Vmdk(w) => w.read_exact_at(offset, dst),
            Self::Vhdx(w) => w.read_exact_at(offset, dst),
        }
    }
}
pub fn native(target: &str, data: &[u8]) {
    native_model(target, data);
}
pub(crate) fn native_model(target: &str, data: &[u8]) -> usize {
    if data.len() > 193 || !cfg!(target_os = "linux") {
        return 0;
    }
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent.img");
    let path = directory.path().join("child.img");
    let overlay = data.first().is_none_or(|b| b & 1 == 0);
    let mut size = if matches!(target, "vdi-write" | "vmdk-write" | "vhdx-write")
        && data.first().is_some_and(|mode| mode & 2 != 0)
    {
        SIZE - 512
    } else {
        SIZE
    };
    let mut model = vec![0; size];
    let mut parents = vec![];
    let original;
    let mut writer = if overlay {
        let base = Writer::create(target, &parent, size as u64);
        model
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = (i % 251) as u8);
        base.writer().write_all_at(0, &model).unwrap();
        base.writer().flush().unwrap();
        drop(base);
        original = Some(std::fs::read(&parent).unwrap());
        parents.push(parent.clone());
        Writer::overlay(target, &path, &parent)
    } else {
        original = None;
        Writer::create(target, &path, size as u64)
    };
    for record in data
        .get(1..)
        .unwrap_or(&[])
        .as_chunks::<6>()
        .0
        .iter()
        .take(32)
    {
        let offset = u16::from_le_bytes([record[1], record[2]]) as usize * 33;
        let length = u16::from_le_bytes([record[3], record[4]]) as usize % 1025;
        let valid = offset.checked_add(length).is_some_and(|end| end <= size);
        match record[0] % 10 {
            0 => {
                let bytes = vec![record[5]; length];
                let result = writer.writer().write_all_at(offset as u64, &bytes);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[offset..offset + length].copy_from_slice(&bytes);
                }
            }
            1 => {
                let result = writer.writer().write_zeroes(offset as u64, length as u64);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    model[offset..offset + length].fill(0);
                }
            }
            2 => {
                writer.writer().flush().unwrap();
                drop(writer);
                writer = Writer::open(target, &path, &parents);
            }
            3 => {
                let mut actual = vec![0; length];
                let result = writer.read(offset as u64, &mut actual);
                assert_eq!(result.is_ok(), valid);
                if valid {
                    assert_eq!(actual, model[offset..offset + length]);
                }
            }
            4 => {
                assert!(writer.writer().write_all_at(u64::MAX, &[1]).is_err());
                assert!(writer.writer().write_zeroes(size as u64 + 1, 0).is_err());
            }
            8 | 9 => {
                let new_size = u16::from_le_bytes([record[1], record[2]]) as usize % 4097 * 512;
                let policy = if record[0] % 10 == 8 {
                    virtdisk::ShrinkPolicy::AllowDataLoss
                } else if record[5] & 1 == 0 {
                    virtdisk::ShrinkPolicy::RequireZero
                } else {
                    virtdisk::ShrinkPolicy::Reject
                };
                let allowed = (new_size == size && (target != "vmdk-write" || !overlay))
                    || (new_size != 0
                        && !overlay
                        && (new_size >= size
                            || policy == virtdisk::ShrinkPolicy::AllowDataLoss
                            || (policy == virtdisk::ShrinkPolicy::RequireZero
                                && model[new_size..].iter().all(|b| *b == 0))));
                let before = if allowed {
                    None
                } else {
                    Some(std::fs::read(&path).unwrap())
                };
                let result = match &mut writer {
                    Writer::Vdi(w) => w.resize(new_size as u64, policy),
                    Writer::Vmdk(w) => w.resize(new_size as u64, policy),
                    Writer::Vhdx(w) => w.resize(new_size as u64, policy),
                };
                assert_eq!(result.is_ok(), allowed);
                if let Some(before) = before {
                    assert_eq!(std::fs::read(&path).unwrap(), before);
                }
                if allowed {
                    model.resize(new_size, 0);
                    size = new_size;
                }
            }
            6 => {
                let start = record[1] as usize % 2 * 1048576;
                let count = size.saturating_sub(start).min(1048576);
                let valid = start <= size;
                let result = writer.writer().discard(
                    start as u64,
                    count as u64,
                    virtdisk::DiscardPolicy::AllowZeroFallback,
                );
                assert_eq!(result.is_ok(), valid);
                if valid {
                    if matches!(target, "vdi-write" | "vmdk-write" | "vhdx-write") && count != 0 {
                        assert_eq!(result.unwrap(), virtdisk::DiscardResult::Deallocated);
                    }
                    model[start..start + count].fill(0);
                }
            }
            7 => {
                let result = writer.writer().discard(
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
                writer.writer().flush().unwrap();
            }
        }
        assert_eq!(writer.writer().len(), size as u64);
        let mut actual = vec![0; size];
        writer.read(0, &mut actual).unwrap();
        assert_eq!(actual, model);
    }
    writer.writer().flush().unwrap();
    drop(writer);
    let writer = Writer::open(target, &path, &parents);
    let mut actual = vec![0; size];
    writer.read(0, &mut actual).unwrap();
    assert_eq!(actual, model);
    if let Some(original) = original {
        assert_eq!(std::fs::read(parent).unwrap(), original);
    }
    size
}
pub fn seeds(target: &str) -> Vec<Vec<u8>> {
    let modes: &[u8] = if matches!(target, "vdi-write" | "vmdk-write" | "vhdx-write") {
        &[0, 1, 2, 3]
    } else {
        &[0, 1]
    };
    let mut seeds: Vec<Vec<u8>> = modes
        .iter()
        .copied()
        .map(|mode| {
            let mut data = vec![mode];
            for record in [
                [0, 31, 124, 16, 0, 9],
                [1, 31, 124, 8, 0, 0],
                [2, 0, 0, 0, 0, 0],
                [0, 254, 255, 1, 0, 7],
                [3, 31, 124, 16, 0, 0],
                [4, 0, 0, 0, 0, 0],
                [5, 0, 0, 0, 0, 0],
                [6, 1, 0, 0, 0, 0],
                [7, 31, 124, 16, 0, 0],
                [2, 0, 0, 0, 0, 0],
            ] {
                data.extend(record);
            }
            data
        })
        .collect();
    if matches!(target, "vdi-write" | "vmdk-write" | "vhdx-write") {
        for mode in [0, 1] {
            let mut data = vec![mode];
            for record in [
                [0, 0, 0, 16, 0, 9],
                [9, 1, 0, 0, 0, 1],
                [9, 1, 0, 0, 0, 0],
                [8, 1, 0, 0, 0, 0],
                [8, 0, 16, 0, 0, 0],
                [6, 1, 0, 0, 0, 0],
                [8, 1, 0, 0, 0, 0],
                [8, 0, 16, 0, 0, 0],
                [2, 0, 0, 0, 0, 0],
            ] {
                data.extend(record);
            }
            seeds.push(data);
        }
    }
    for mode in [0, 1, 2, 3] {
        let mut data = vec![mode];
        for record in [
            [6, 0, 0, 0, 0, 0],
            [0, 1, 0, 16, 0, 73],
            [2, 0, 0, 0, 0, 0],
            [6, 0, 0, 0, 0, 0],
            [3, 1, 0, 16, 0, 0],
            [6, 1, 0, 0, 0, 0],
            [2, 0, 0, 0, 0, 0],
        ] {
            data.extend(record);
        }
        seeds.push(data);
    }
    seeds
}
