//! Bounded QCOW2 allocation transaction records.

use crate::ReadAt;
use sha2::{Digest, Sha256};
use std::io;

const LIMIT: usize = 4 * 1024 * 1024;
const PATCH_LIMIT: usize = 16;
const MAGIC: &[u8; 8] = b"VDQCJ001";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Patch {
    pub(super) offset: u64,
    pub(super) old: Vec<u8>,
    pub(super) new: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record {
    pub(super) original_length: u64,
    pub(super) final_length: u64,
    pub(super) original_digest: [u8; 32],
    pub(super) patches: Vec<Patch>,
}

fn corrupt() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid QCOW2 transaction journal",
    )
}

impl Record {
    fn validate(&self) -> io::Result<()> {
        if self.final_length < self.original_length
            || self.final_length > 33 * 1024 * 1024 * 1024
            || self.patches.is_empty()
            || self.patches.len() > PATCH_LIMIT
        {
            return Err(corrupt());
        }
        let mut previous_end = 0;
        for patch in &self.patches {
            let end = patch
                .offset
                .checked_add(patch.new.len() as u64)
                .ok_or_else(corrupt)?;
            let old_length = self
                .original_length
                .saturating_sub(patch.offset)
                .min(patch.new.len() as u64);
            if patch.new.is_empty()
                || patch.new.len() > 65536
                || patch.offset < previous_end
                || end > self.final_length
                || patch.old.len() as u64 != old_length
            {
                return Err(corrupt());
            }
            previous_end = end;
        }
        Ok(())
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.original_length.to_be_bytes());
        bytes.extend_from_slice(&self.final_length.to_be_bytes());
        bytes.extend_from_slice(&self.original_digest);
        bytes.extend_from_slice(&(self.patches.len() as u64).to_be_bytes());
        for patch in &self.patches {
            bytes.extend_from_slice(&patch.offset.to_be_bytes());
            bytes.extend_from_slice(&(patch.old.len() as u64).to_be_bytes());
            bytes.extend_from_slice(&(patch.new.len() as u64).to_be_bytes());
            bytes.extend_from_slice(&patch.old);
            bytes.extend_from_slice(&patch.new);
        }
        if bytes.len() + 32 > LIMIT {
            return Err(corrupt());
        }
        let checksum = Sha256::digest(&bytes);
        bytes.extend_from_slice(&checksum);
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 96 || bytes.len() > LIMIT || &bytes[..8] != MAGIC {
            return Err(corrupt());
        }
        let body_length = bytes.len() - 32;
        if Sha256::digest(&bytes[..body_length])[..] != bytes[body_length..] {
            return Err(corrupt());
        }
        let mut cursor = 8;
        let original_length = take_u64(bytes, &mut cursor)?;
        let final_length = take_u64(bytes, &mut cursor)?;
        let original_digest = bytes[cursor..cursor + 32]
            .try_into()
            .map_err(|_| corrupt())?;
        cursor += 32;
        let count = take_u64(bytes, &mut cursor)?;
        if count == 0 || count > PATCH_LIMIT as u64 {
            return Err(corrupt());
        }
        let mut patches = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let offset = take_u64(bytes, &mut cursor)?;
            let old_length = take_u64(bytes, &mut cursor)?;
            let new_length = take_u64(bytes, &mut cursor)?;
            if old_length > 65536 || new_length > 65536 {
                return Err(corrupt());
            }
            let end_old = cursor
                .checked_add(old_length as usize)
                .ok_or_else(corrupt)?;
            let end_new = end_old
                .checked_add(new_length as usize)
                .ok_or_else(corrupt)?;
            if end_new > body_length {
                return Err(corrupt());
            }
            patches.push(Patch {
                offset,
                old: bytes[cursor..end_old].to_vec(),
                new: bytes[end_old..end_new].to_vec(),
            });
            cursor = end_new;
        }
        if cursor != body_length {
            return Err(corrupt());
        }
        let record = Self {
            original_length,
            final_length,
            original_digest,
            patches,
        };
        record.validate()?;
        Ok(record)
    }
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> io::Result<u64> {
    let end = cursor.checked_add(8).ok_or_else(corrupt)?;
    let slice = bytes.get(*cursor..end).ok_or_else(corrupt)?;
    *cursor = end;
    Ok(u64::from_be_bytes(slice.try_into().map_err(|_| corrupt())?))
}

pub(super) fn digest_reader(source: &dyn ReadAt) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    let mut position = 0;
    while position < source.len() {
        let count = (source.len() - position).min(buffer.len() as u64) as usize;
        source.read_exact_at(position, &mut buffer[..count])?;
        hash.update(&buffer[..count]);
        position += count as u64;
    }
    Ok(hash.finalize().into())
}

impl Record {
    fn validate_original(&self, source: &dyn ReadAt) -> io::Result<()> {
        self.validate()?;
        if source.len() < self.original_length || source.len() > self.final_length {
            return Err(corrupt());
        }
        for patch in &self.patches {
            let count = source
                .len()
                .saturating_sub(patch.offset)
                .min(patch.new.len() as u64) as usize;
            let mut current = vec![0; count];
            if count != 0 {
                source.read_exact_at(patch.offset, &mut current)?;
            }
            for (index, mut byte) in current.into_iter().enumerate() {
                if patch.offset + index as u64 == 79 {
                    byte &= !1;
                }
                let old = patch.old.get(index).copied().unwrap_or(0);
                let mut new = patch.new[index];
                if patch.offset + index as u64 == 79 {
                    new &= !1;
                }
                if byte != old && byte != new {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "QCOW2 journal target range differs from transaction states",
                    ));
                }
            }
        }
        let reconstructed = Patched {
            source,
            record: self,
            replacement: false,
        };
        if digest_reader(&reconstructed)? != self.original_digest {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "QCOW2 journal image identity mismatch",
            ));
        }
        Ok(())
    }
}

struct Patched<'a> {
    source: &'a dyn ReadAt,
    record: &'a Record,
    replacement: bool,
}
impl ReadAt for Patched<'_> {
    fn len(&self) -> u64 {
        if self.replacement {
            self.record.final_length
        } else {
            self.record.original_length
        }
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, dst.len() as u64, self.len())?;
        let mut done = 0;
        while done < dst.len() {
            let position = offset + done as u64;
            let patch = self.record.patches.iter().find(|patch| {
                position >= patch.offset && position < patch.offset + patch.new.len() as u64
            });
            let count;
            if let Some(patch) = patch {
                let data = if self.replacement {
                    &patch.new
                } else {
                    &patch.old
                };
                let start = (position - patch.offset) as usize;
                count = (data.len() - start).min(dst.len() - done);
                dst[done..done + count].copy_from_slice(&data[start..start + count]);
            } else {
                let next = self
                    .record
                    .patches
                    .iter()
                    .filter(|patch| patch.offset > position)
                    .map(|patch| patch.offset)
                    .min()
                    .unwrap_or(self.len());
                count = (next - position).min((dst.len() - done) as u64) as usize;
                self.source
                    .read_exact_at(position, &mut dst[done..done + count])?;
            }
            done += count;
        }
        // Only the native dirty feature is normalized; all other feature bits
        // remain part of the original identity and proposed validation.
        if offset <= 79 && offset + dst.len() as u64 > 79 {
            dst[(79 - offset) as usize] &= !1;
        }
        Ok(())
    }
}

pub(super) fn sidecar(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".virtdisk-qcow2-journal");
    name.into()
}

fn sync_parent(path: &std::path::Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        std::fs::File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new(".")),
        )?
        .sync_all()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "QCOW2 journal directory durability currently requires Linux",
        ))
    }
}

struct OwnedPatched {
    source: std::sync::Arc<dyn ReadAt>,
    record: std::sync::Arc<Record>,
    replacement: bool,
}
impl ReadAt for OwnedPatched {
    fn len(&self) -> u64 {
        if self.replacement {
            self.record.final_length
        } else {
            self.record.original_length
        }
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        Patched {
            source: &*self.source,
            record: &self.record,
            replacement: self.replacement,
        }
        .read_exact_at(offset, dst)
    }
}

fn validate_states(
    path: &std::path::Path,
    authorized: &[std::path::PathBuf],
    raw: std::sync::Arc<crate::RawWriter>,
    record: &Record,
) -> io::Result<()> {
    let identity = raw.file_identity()?;
    let source: std::sync::Arc<dyn ReadAt> = std::sync::Arc::new(super::LockedSource {
        size: raw.len(),
        raw,
    });
    record.validate_original(&*source)?;
    let record = std::sync::Arc::new(record.clone());
    for replacement in [false, true] {
        let view = std::sync::Arc::new(OwnedPatched {
            source: source.clone(),
            record: record.clone(),
            replacement,
        });
        crate::Qcow2::open_locked_chain(view, path, authorized, identity)?
            .validate_active_mapping()?;
    }
    Ok(())
}

fn interrupt(stage: usize, cut: Option<usize>) -> io::Result<()> {
    if cut == Some(stage) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "injected QCOW2 transaction interruption",
        ))
    } else {
        Ok(())
    }
}

fn replay(
    path: &std::path::Path,
    raw: std::sync::Arc<crate::RawWriter>,
    record: &Record,
    cut: Option<usize>,
    authorized: &[std::path::PathBuf],
) -> io::Result<()> {
    validate_states(path, authorized, raw.clone(), record)?;
    let mut feature = [0];
    raw.read_exact_at(79, &mut feature)?;
    feature[0] |= 1;
    raw.write_all_at(79, &feature)?;
    interrupt(6, cut)?;
    raw.flush()?;
    interrupt(1, cut)?;
    raw.resize(record.final_length)?;
    interrupt(2, cut)?;
    for (index, patch) in record.patches.iter().rev().enumerate() {
        raw.write_all_at(patch.offset, &patch.new)?;
        // Header patches must not clear the dirty bit before the sync barrier.
        if patch.offset <= 79 && patch.offset + patch.new.len() as u64 > 79 {
            raw.write_all_at(79, &feature)?;
        }
        interrupt(100 + index, cut)?;
    }
    interrupt(3, cut)?;
    raw.flush()?;
    interrupt(4, cut)?;
    feature[0] &= !1;
    raw.write_all_at(79, &feature)?;
    interrupt(7, cut)?;
    raw.flush()?;
    interrupt(5, cut)?;
    std::fs::remove_file(sidecar(path))?;
    sync_parent(path)
}

pub(super) fn commit_authorized(
    path: &std::path::Path,
    raw: std::sync::Arc<crate::RawWriter>,
    record: Record,
    cut: Option<usize>,
    authorized: &[std::path::PathBuf],
) -> io::Result<()> {
    use std::io::Write;
    validate_states(path, authorized, raw.clone(), &record)?;
    sync_parent(path)?; // Reject unsupported directory persistence before mutation.
    let bytes = record.encode()?;
    let journal = sidecar(path);
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut temporary = journal.as_os_str().to_owned();
    temporary.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let temporary = std::path::PathBuf::from(temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::hard_link(&temporary, &journal)?; // Atomic no-overwrite publication.
    sync_parent(path)?;
    std::fs::remove_file(&temporary)?;
    interrupt(0, cut)?;
    replay(path, raw, &record, cut, authorized)
}

pub(super) fn recover_authorized(
    path: &std::path::Path,
    raw: std::sync::Arc<crate::RawWriter>,
    authorized: &[std::path::PathBuf],
) -> io::Result<()> {
    use std::io::Read;
    let journal = sidecar(path);
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000);
    } // O_NOFOLLOW
    let mut file = match options.open(journal) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut dirty = [0];
            raw.read_exact_at(79, &mut dirty)?;
            return if dirty[0] & 1 != 0 {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "dirty QCOW2 image requires its transaction journal",
                ))
            } else {
                Ok(())
            };
        }
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() || file.metadata()?.len() > LIMIT as u64 {
        return Err(corrupt());
    }
    let mut bytes = Vec::new();
    (&mut file).take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    let record = Record::decode(&bytes)?;
    sync_parent(path)?;
    replay(path, raw, &record, None, authorized)
}

#[cfg(all(test, target_os = "linux"))]
fn commit(
    path: &std::path::Path,
    raw: std::sync::Arc<crate::RawWriter>,
    record: Record,
    cut: Option<usize>,
) -> io::Result<()> {
    commit_authorized(path, raw, record, cut, &[])
}
#[cfg(all(test, target_os = "linux"))]
fn recover(path: &std::path::Path, raw: std::sync::Arc<crate::RawWriter>) -> io::Result<()> {
    recover_authorized(path, raw, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn interruptions_recover_idempotently_and_preserve_valid_qcow2() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in (0..=7).chain(100..101) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.qcow2");
            drop(super::super::Qcow2Writer::create(&path, 65536).unwrap());
            let raw = std::sync::Arc::new(crate::RawWriter::open(&path).unwrap());
            let source: std::sync::Arc<dyn crate::ReadAt> =
                std::sync::Arc::new(super::super::LockedSource {
                    raw: raw.clone(),
                    size: raw.len(),
                });
            let record = Record {
                original_length: raw.len(),
                final_length: raw.len(),
                original_digest: digest_reader(&*source).unwrap(),
                patches: vec![Patch {
                    offset: 5 * 65536,
                    old: vec![0; 512],
                    new: vec![7; 512],
                }],
            };
            assert!(commit(&path, raw.clone(), record, Some(cut)).is_err());
            recover(&path, raw.clone()).unwrap();
            recover(&path, raw.clone()).unwrap();
            let mut actual = [0; 512];
            raw.read_exact_at(5 * 65536, &mut actual).unwrap();
            assert_eq!(actual, [7; 512]);
            let reader = std::sync::Arc::new(super::super::LockedSource {
                raw: raw.clone(),
                size: raw.len(),
            });
            crate::Qcow2::open(reader)
                .unwrap()
                .validate_active_mapping()
                .unwrap();
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn invalid_sidecars_do_not_mutate_image_or_remove_evidence() {
        let _process_boundary = crate::test_sync::writer_test();
        for foreign in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.qcow2");
            drop(super::super::Qcow2Writer::create(&path, 65536).unwrap());
            let raw = std::sync::Arc::new(crate::RawWriter::open(&path).unwrap());
            let source = super::super::LockedSource {
                raw: raw.clone(),
                size: raw.len(),
            };
            let record = Record {
                original_length: raw.len(),
                final_length: raw.len(),
                original_digest: digest_reader(&source).unwrap(),
                patches: vec![Patch {
                    offset: 5 * 65536,
                    old: vec![0; 512],
                    new: vec![7; 512],
                }],
            };
            let mut bytes = record.encode().unwrap();
            if foreign {
                raw.write_all_at(5 * 65536 + 900, &[3]).unwrap();
            } else {
                bytes[16] ^= 1;
            }
            std::fs::write(sidecar(&path), &bytes).unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(recover(&path, raw).is_err());
            assert!(std::fs::read(&path).unwrap() == before);
            assert!(std::fs::read(sidecar(&path)).unwrap() == bytes);
        }
    }

    #[test]
    fn journal_roundtrip_and_checksum_rejection() {
        let _process_boundary = crate::test_sync::writer_test();
        let record = Record {
            original_length: 65536,
            final_length: 131072,
            original_digest: [7; 32],
            patches: vec![
                Patch {
                    offset: 0,
                    old: vec![1, 2],
                    new: vec![3, 4],
                },
                Patch {
                    offset: 65536,
                    old: vec![],
                    new: vec![9; 512],
                },
            ],
        };
        let bytes = record.encode().unwrap();
        assert_eq!(Record::decode(&bytes).unwrap(), record);
        for cut in [0, 7, 48, bytes.len() - 1] {
            assert!(Record::decode(&bytes[..cut]).is_err());
        }
        let mut corrupt = bytes;
        corrupt[16] ^= 1;
        assert!(Record::decode(&corrupt).is_err());
    }

    #[test]
    fn original_digest_survives_torn_updates_but_rejects_foreign_images() {
        let _process_boundary = crate::test_sync::writer_test();
        struct Bytes(Vec<u8>);
        impl crate::ReadAt for Bytes {
            fn len(&self) -> u64 {
                self.0.len() as u64
            }
            fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
                dst.copy_from_slice(
                    self.0
                        .get(offset as usize..offset as usize + dst.len())
                        .ok_or_else(corrupt)?,
                );
                Ok(())
            }
        }
        let original = Bytes(vec![0; 65536]);
        let record = Record {
            original_length: 65536,
            final_length: 131072,
            original_digest: digest_reader(&original).unwrap(),
            patches: vec![
                Patch {
                    offset: 1024,
                    old: vec![0; 512],
                    new: vec![7; 512],
                },
                Patch {
                    offset: 65536,
                    old: vec![],
                    new: vec![9; 65536],
                },
            ],
        };
        let mut current = Bytes(vec![0; 131072]);
        current.0[1024..1200].fill(7);
        current.0[79] = 1; // transient native dirty flag
        current.0[65536..66000].fill(9);
        record.validate_original(&current).unwrap();
        let view = Patched {
            source: &current,
            record: &record,
            replacement: true,
        };
        let mut result = vec![0; 131072];
        view.read_exact_at(0, &mut result).unwrap();
        assert!(result[1024..1536].iter().all(|byte| *byte == 7));
        assert_eq!(result[79], 0);
        assert!(result[65536..].iter().all(|byte| *byte == 9));
        current.0[900] = 3;
        assert!(record.validate_original(&current).is_err());
    }

    #[test]
    fn rejects_overlaps_overflows_and_malformed_replacement_lengths() {
        let _process_boundary = crate::test_sync::writer_test();
        for patches in [
            vec![
                Patch {
                    offset: 0,
                    old: vec![1; 8],
                    new: vec![2; 8],
                },
                Patch {
                    offset: 4,
                    old: vec![1; 8],
                    new: vec![2; 8],
                },
            ],
            vec![Patch {
                offset: u64::MAX,
                old: vec![],
                new: vec![1; 2],
            }],
            vec![Patch {
                offset: 1,
                old: vec![1],
                new: vec![2; 2],
            }],
        ] {
            assert!(
                Record {
                    original_length: 65536,
                    final_length: 131072,
                    original_digest: [0; 32],
                    patches
                }
                .encode()
                .is_err()
            );
        }
    }
}
