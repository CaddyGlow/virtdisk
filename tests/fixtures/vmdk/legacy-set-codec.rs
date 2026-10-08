// Frozen standalone VDTXSET1 codec from the multiple-table checkpoint.
use crate::transaction::Record;
use sha2::{Digest, Sha256};
use std::{io, path::{Path, PathBuf}};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
const MAGIC: &[u8; 8] = b"VDTXSET1";
const MARKER: &[u8; 8] = b"VDTXPAR1";
const LIMIT: usize = 4 * 1024 * 1024;
const MAX_FILES: usize = 257;
fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid hosted image transaction set") }
#[derive(Clone)]
struct Participant {
    path: PathBuf,
    identity: (u64, u64),
    length: u64,
    digest: [u8; 32],
    record: Option<Record>,
}
struct Set {
    id: [u8; 16],
    participants: Vec<Participant>,
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, size: usize) -> io::Result<&'a [u8]> {
        let end = self.at.checked_add(size).ok_or_else(invalid)?;
        let out = self.bytes.get(self.at..end).ok_or_else(invalid)?;
        self.at = end;
        Ok(out)
    }
    fn number(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| invalid())?,
        ))
    }
}
impl Set {
    fn encode(&self) -> io::Result<Vec<u8>> {
        if self.participants.is_empty() || self.participants.len() > MAX_FILES {
            return Err(invalid());
        }
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&self.id);
        push_u64(&mut out, self.participants.len() as u64);
        let mut patches = 0;
        let mut physical = 0u64;
        for participant in &self.participants {
            physical = physical
                .checked_add(
                    participant
                        .record
                        .as_ref()
                        .map_or(participant.length, |record| {
                            record.final_length.max(participant.length)
                        }),
                )
                .ok_or_else(invalid)?;
            if physical > 33 * 1024 * 1024 * 1024 {
                return Err(invalid());
            }
            let path = participant.path.as_os_str().as_bytes();
            if !participant.path.is_absolute()
                || path.is_empty()
                || path.len() > 4096
                || path.contains(&0)
            {
                return Err(invalid());
            }
            push_u64(&mut out, path.len() as u64);
            out.extend_from_slice(path);
            push_u64(&mut out, participant.identity.0);
            push_u64(&mut out, participant.identity.1);
            push_u64(&mut out, participant.length);
            out.extend_from_slice(&participant.digest);
            let bytes = match &participant.record {
                Some(record) => {
                    patches += record.patches.len();
                    if record.original_length != participant.length
                        || record.original_digest != participant.digest
                        || record.final_length < record.original_length
                    {
                        return Err(invalid());
                    }
                    record.encode()?
                }
                None => Vec::new(),
            };
            if patches > 16 {
                return Err(invalid());
            }
            push_u64(&mut out, bytes.len() as u64);
            out.extend_from_slice(&bytes);
            if out.len() + 32 > LIMIT {
                return Err(invalid());
            }
        }
        let checksum = Sha256::digest(&out);
        out.extend_from_slice(&checksum);
        Ok(out)
    }
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 64 || bytes.len() > LIMIT {
            return Err(invalid());
        }
        let body = &bytes[..bytes.len() - 32];
        if Sha256::digest(body)[..] != bytes[bytes.len() - 32..] {
            return Err(invalid());
        }
        let mut cursor = Cursor { bytes: body, at: 0 };
        if cursor.take(8)? != MAGIC {
            return Err(invalid());
        }
        let id = cursor.take(16)?.try_into().map_err(|_| invalid())?;
        let count = cursor.number()?;
        if count == 0 || count > MAX_FILES as u64 {
            return Err(invalid());
        }
        let mut participants = Vec::new();
        for _ in 0..count {
            let size = cursor.number()?;
            if size == 0 || size > 4096 {
                return Err(invalid());
            }
            let path = PathBuf::from(std::ffi::OsString::from_vec(
                cursor.take(size as usize)?.to_vec(),
            ));
            let identity = (cursor.number()?, cursor.number()?);
            let length = cursor.number()?;
            let digest = cursor.take(32)?.try_into().map_err(|_| invalid())?;
            let size = cursor.number()?;
            if size > LIMIT as u64 {
                return Err(invalid());
            }
            let record = if size == 0 {
                None
            } else {
                Some(Record::decode(cursor.take(size as usize)?)?)
            };
            if participants
                .iter()
                .any(|p: &Participant| p.path == path || p.identity == identity)
            {
                return Err(invalid());
            }
            participants.push(Participant {
                path,
                identity,
                length,
                digest,
                record,
            });
        }
        if cursor.at != body.len() {
            return Err(invalid());
        }
        let set = Self { id, participants };
        set.encode()?;
        Ok(set)
    }
    fn marker(&self, descriptor: &Path) -> io::Result<Vec<u8>> {
        let path = descriptor.as_os_str().as_bytes();
        if path.len() > 4096 {
            return Err(invalid());
        }
        let mut bytes = MARKER.to_vec();
        bytes.extend_from_slice(&self.id);
        push_u64(&mut bytes, path.len() as u64);
        bytes.extend_from_slice(path);
        let digest = Sha256::digest(&bytes);
        bytes.extend_from_slice(&digest);
        Ok(bytes)
    }
}

pub(super) fn accepts(bytes: &[u8]) -> bool { Set::decode(bytes).is_ok() }
