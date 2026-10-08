//! Bounded hosted-image metadata transactions with old/proposed validation.

use crate::ReadAt;
use crate::io;
use crate::source::LockedSource;
use sha2::{Digest, Sha256};

const LIMIT: usize = 4 * 1024 * 1024;
const PATCH_LIMIT: usize = 16;
const MAGIC: &[u8; 8] = b"VDTXJ002";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Patch {
    pub(crate) order: u32,
    pub(crate) offset: u64,
    pub(crate) old: Vec<u8>,
    pub(crate) new: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) original_length: u64,
    pub(crate) final_length: u64,
    pub(crate) original_digest: [u8; 32],
    pub(crate) patches: Vec<Patch>,
}

fn corrupt() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid hosted image transaction journal",
    )
}

impl Record {
    fn validate(&self) -> io::Result<()> {
        if self.original_length.max(self.final_length) > 33 * 1024 * 1024 * 1024
            || self.patches.is_empty()
            || self.patches.len() > PATCH_LIMIT
        {
            return Err(corrupt());
        }
        let shrinking = self.final_length < self.original_length;
        let mut archived = false;
        let mut previous_end = 0;
        let mut orders = [false; PATCH_LIMIT];
        for patch in &self.patches {
            if patch.order as usize >= self.patches.len() || orders[patch.order as usize] {
                return Err(corrupt());
            }
            orders[patch.order as usize] = true;
            let tombstone = shrinking && patch.new.is_empty() && patch.offset == self.final_length;
            let span = patch.old.len().max(patch.new.len()) as u64;
            let end = patch.offset.checked_add(span).ok_or_else(corrupt)?;
            if patch.offset < previous_end || span == 0 || span > 1048576 {
                return Err(corrupt());
            }
            if tombstone {
                if archived || end != self.original_length {
                    return Err(corrupt());
                }
                archived = true;
            } else {
                let old_length = self
                    .original_length
                    .saturating_sub(patch.offset)
                    .min(patch.new.len() as u64);
                if patch.new.is_empty()
                    || end > self.final_length
                    || patch.old.len() as u64 != old_length
                {
                    return Err(corrupt());
                }
            }
            previous_end = end;
        }
        if shrinking && !archived {
            return Err(corrupt());
        }
        Ok(())
    }

    pub(crate) fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.original_length.to_be_bytes());
        bytes.extend_from_slice(&self.final_length.to_be_bytes());
        bytes.extend_from_slice(&self.original_digest);
        bytes.extend_from_slice(&(self.patches.len() as u64).to_be_bytes());
        for patch in &self.patches {
            bytes.extend_from_slice(&u64::from(patch.order).to_be_bytes());
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

    pub(crate) fn decode(bytes: &[u8]) -> io::Result<Self> {
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
            let order = u32::try_from(take_u64(bytes, &mut cursor)?).map_err(|_| corrupt())?;
            let offset = take_u64(bytes, &mut cursor)?;
            let old_length = take_u64(bytes, &mut cursor)?;
            let new_length = take_u64(bytes, &mut cursor)?;
            if old_length > 1048576 || new_length > 1048576 {
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
                order,
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

pub(crate) fn digest_reader(source: &dyn ReadAt) -> io::Result<[u8; 32]> {
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
    pub(crate) fn validate_original(&self, source: &dyn ReadAt) -> io::Result<()> {
        self.validate()?;
        if source.len() < self.original_length.min(self.final_length)
            || source.len() > self.original_length.max(self.final_length)
        {
            return Err(corrupt());
        }
        for patch in &self.patches {
            let count = source
                .len()
                .saturating_sub(patch.offset)
                .min(patch.new.len().max(patch.old.len()) as u64) as usize;
            let mut current = vec![0; count];
            if count != 0 {
                source.read_exact_at(patch.offset, &mut current)?;
            }
            for (index, byte) in current.into_iter().enumerate() {
                let old = patch.old.get(index).copied().unwrap_or(0);
                let new = patch.new.get(index).copied().unwrap_or(old);
                if byte != old && byte != new {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "hosted image journal target range differs from transaction states",
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
                "hosted image journal image identity mismatch",
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
                position >= patch.offset
                    && position
                        < patch.offset
                            + if self.replacement {
                                patch.new.len()
                            } else {
                                patch.old.len()
                            } as u64
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
        Ok(())
    }
}

pub(crate) fn sidecar(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".virtdisk-transaction");
    name.into()
}

pub(crate) fn pending(path: &std::path::Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(sidecar(path)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn sync_parent(path: &std::path::Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        Ok(std::fs::File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new(".")),
        )?
        .sync_all()?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "hosted image journal directory durability currently requires Linux",
        ))
    }
}

struct OwnedPatched {
    source: std::sync::Arc<dyn ReadAt>,
    record: std::sync::Arc<Record>,
    replacement: bool,
}

#[cfg(target_os = "linux")]
pub(crate) fn shadow(
    source: std::sync::Arc<dyn ReadAt>,
    record: std::sync::Arc<Record>,
    replacement: bool,
) -> std::sync::Arc<dyn ReadAt> {
    std::sync::Arc::new(OwnedPatched {
        source,
        record,
        replacement,
    })
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
    raw: std::sync::Arc<crate::RawWriter>,
    record: &Record,
    validator: &dyn Fn(std::sync::Arc<dyn ReadAt>) -> io::Result<()>,
) -> io::Result<()> {
    let source: std::sync::Arc<dyn ReadAt> = std::sync::Arc::new(LockedSource {
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
        validator(view)?;
    }
    Ok(())
}

fn interrupt(stage: usize, cut: Option<usize>) -> io::Result<()> {
    if cut == Some(stage) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "injected hosted image transaction interruption",
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
    validator: &dyn Fn(std::sync::Arc<dyn ReadAt>) -> io::Result<()>,
) -> io::Result<()> {
    validate_states(raw.clone(), record, validator)?;
    interrupt(1, cut)?;
    let shrinking = record.final_length < record.original_length;
    if !shrinking {
        raw.resize(record.final_length)?;
        interrupt(2, cut)?;
    }
    let mut ordered: Vec<_> = record.patches.iter().collect();
    ordered.sort_unstable_by_key(|patch| patch.order);
    for (index, patch) in ordered.into_iter().enumerate() {
        if !patch.new.is_empty() {
            raw.write_all_at(patch.offset, &patch.new)?;
        }
        interrupt(6 + index, cut)?;
    }
    interrupt(3, cut)?;
    raw.flush()?;
    interrupt(4, cut)?;
    if shrinking {
        raw.resize(record.final_length)?;
        raw.flush()?;
        interrupt(2, cut)?;
    }
    interrupt(5, cut)?;
    std::fs::remove_file(sidecar(path))?;
    sync_parent(path)
}

pub(crate) fn commit(
    path: &std::path::Path,
    raw: std::sync::Arc<crate::RawWriter>,
    record: Record,
    cut: Option<usize>,
    validator: &dyn Fn(std::sync::Arc<dyn ReadAt>) -> io::Result<()>,
) -> io::Result<()> {
    use crate::io::Write;
    raw.require_single_link_for_journal()?;
    validate_states(raw.clone(), &record, validator)?;
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
    replay(path, raw, &record, cut, validator)
}

pub(crate) fn recover(
    path: &std::path::Path,
    raw: std::sync::Arc<crate::RawWriter>,
    validator: &dyn Fn(std::sync::Arc<dyn ReadAt>) -> io::Result<()>,
) -> io::Result<()> {
    raw.require_single_link_for_journal()?;
    let Some(bytes) = crate::sidecar::read_bounded(&sidecar(path), LIMIT, corrupt)? else {
        return Ok(());
    };
    if bytes.starts_with(b"VDTXPAR1") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "image is a multi-file transaction participant; reopen the authorized VMDK descriptor to recover",
        ));
    }
    let record = Record::decode(&bytes)?;
    sync_parent(path)?;
    replay(path, raw, &record, None, validator)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(target_os = "linux")]
    fn shadow_views_share_record_and_select_old_or_new_bytes() {
        let record = std::sync::Arc::new(Record {
            original_length: 4,
            final_length: 4,
            original_digest: [0; 32],
            patches: vec![Patch {
                order: 0,
                offset: 0,
                old: vec![1; 4],
                new: vec![2; 4],
            }],
        });
        let source: std::sync::Arc<dyn ReadAt> = std::sync::Arc::new(crate::source::ZeroSource(4));
        let old = shadow(source.clone(), record.clone(), false);
        let new = shadow(source, record.clone(), true);
        assert_eq!(std::sync::Arc::strong_count(&record), 3);
        let mut bytes = [0; 4];
        old.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [1; 4]);
        new.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [2; 4]);
    }

    #[test]
    fn bounded_record_rejects_overlaps_bad_lengths_checksum_and_truncation() {
        let good = Record {
            original_length: 512,
            final_length: 1024,
            original_digest: [7; 32],
            patches: vec![
                Patch {
                    order: 1,
                    offset: 32,
                    old: vec![0; 4],
                    new: vec![1; 4],
                },
                Patch {
                    order: 0,
                    offset: 512,
                    old: vec![],
                    new: vec![2; 512],
                },
            ],
        };
        let bytes = good.encode().unwrap();
        assert_eq!(Record::decode(&bytes).unwrap(), good);
        for cut in [0, 7, 95, bytes.len() - 1] {
            assert!(Record::decode(&bytes[..cut]).is_err());
        }
        let mut bad = bytes;
        bad[24] ^= 1;
        assert!(Record::decode(&bad).is_err());
        let mut bad = good.clone();
        bad.patches[1].offset = 33;
        assert!(bad.encode().is_err());
        let mut bad = good.clone();
        bad.patches[0].old.clear();
        assert!(bad.encode().is_err());
        let mut bad = good;
        bad.patches[1].offset = u64::MAX;
        assert!(bad.encode().is_err());
    }
    #[test]
    fn bounded_shrink_archive_reconstructs_preimage_and_rejects_foreign_tail() {
        struct Bytes(Vec<u8>);
        impl ReadAt for Bytes {
            fn len(&self) -> u64 {
                self.0.len() as u64
            }
            fn read_exact_at(&self, o: u64, b: &mut [u8]) -> io::Result<()> {
                crate::check_range(o, b.len() as u64, self.len())?;
                b.copy_from_slice(&self.0[o as usize..o as usize + b.len()]);
                Ok(())
            }
        }
        let original = Bytes(vec![7; 1024]);
        let record = Record {
            original_length: 1024,
            final_length: 512,
            original_digest: digest_reader(&original).unwrap(),
            patches: vec![
                Patch {
                    order: 0,
                    offset: 0,
                    old: vec![7; 4],
                    new: vec![9; 4],
                },
                Patch {
                    order: 1,
                    offset: 512,
                    old: vec![7; 512],
                    new: vec![],
                },
            ],
        };
        let encoded = record.encode().expect("bounded shrink archive");
        assert_eq!(Record::decode(&encoded).unwrap(), record);
        for length in [512, 700, 1024] {
            let mut current = vec![7; length];
            current[..4].fill(9);
            record.validate_original(&Bytes(current)).unwrap();
        }
        let mut foreign = vec![7; 1024];
        foreign[900] = 8;
        assert!(record.validate_original(&Bytes(foreign)).is_err());
        let mut foreign = vec![7; 512];
        foreign[100] = 8;
        assert!(record.validate_original(&Bytes(foreign)).is_err());
        for mutation in 0..4 {
            let mut bad = record.clone();
            match mutation {
                0 => bad.patches[1].old.pop(),
                1 => {
                    bad.patches[1].offset += 1;
                    None
                }
                2 => {
                    bad.patches[1].new.push(1);
                    None
                }
                _ => {
                    bad.patches.pop();
                    None
                }
            };
            assert!(bad.encode().is_err());
        }
    }
}
