//! Bounded protocol and PE import validation for the isolated process-kill gate.
use std::io;
fn bad() -> io::Error {
    io::Error::other("invalid bounded process-gate data")
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub volume: u32,
    pub file: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    pub kind: u32,
    pub cut: u32,
    pub identity: Identity,
}
impl Ack {
    pub fn ready(identity: Identity) -> Self {
        Self {
            kind: 1,
            cut: 0,
            identity,
        }
    }
    pub fn flushed(cut: u32, identity: Identity) -> Self {
        Self {
            kind: 2,
            cut,
            identity,
        }
    }
    pub fn done(cut: u32, identity: Identity) -> Self {
        Self {
            kind: 3,
            cut,
            identity,
        }
    }
    pub fn encode(self) -> [u8; 32] {
        let mut b = [0; 32];
        b[..8].copy_from_slice(b"VDGATE01");
        b[8..12].copy_from_slice(&self.kind.to_le_bytes());
        b[12..16].copy_from_slice(&self.cut.to_le_bytes());
        b[16..20].copy_from_slice(&self.identity.volume.to_le_bytes());
        b[20..28].copy_from_slice(&self.identity.file.to_le_bytes());
        b
    }
    pub fn decode(b: &[u8]) -> io::Result<Self> {
        if b.len() != 32 || &b[..8] != b"VDGATE01" || b[28..32] != [0; 4] {
            return Err(bad());
        }
        let n = |o| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let a = Self {
            kind: n(8),
            cut: n(12),
            identity: Identity {
                volume: n(16),
                file: u64::from_le_bytes(b[20..28].try_into().unwrap()),
            },
        };
        if !(1..=3).contains(&a.kind)
            || a.cut > 128
            || (a.kind == 1 && a.cut != 0)
            || (a.kind == 2 && a.cut == 0)
        {
            return Err(bad());
        }
        Ok(a)
    }
}
pub struct Policy {
    identity: Identity,
    ready: bool,
    done: bool,
    cut: u32,
}
impl Policy {
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            ready: false,
            done: false,
            cut: 0,
        }
    }
    pub fn observe(&mut self, a: Ack) -> io::Result<()> {
        if a.identity != self.identity || self.done {
            return Err(bad());
        }
        match a.kind {
            1 if !self.ready && a.cut == 0 => self.ready = true,
            2 if self.ready && a.cut == self.cut + 1 && a.cut <= 128 => self.cut = a.cut,
            3 if self.ready && a.cut == self.cut => self.done = true,
            _ => return Err(bad()),
        }
        Ok(())
    }
    pub fn cuts(&self) -> u32 {
        self.cut
    }
}
pub fn find_flush_iat(image: &[u8]) -> io::Result<usize> {
    find_flush_iat_with(|at, n| {
        image
            .get(at..at.checked_add(n).ok_or_else(bad)?)
            .map(|b| b.to_vec())
            .ok_or_else(bad)
    })
}
pub(crate) fn find_flush_iat_with(
    read: impl Fn(usize, usize) -> io::Result<Vec<u8>>,
) -> io::Result<usize> {
    let h = read(0, 4096)?;
    let u16_at = |o| u16::from_le_bytes(h[o..o + 2].try_into().unwrap());
    let u32_at = |o| u32::from_le_bytes(h[o..o + 4].try_into().unwrap());
    if &h[..2] != b"MZ" {
        return Err(bad());
    }
    let pe = u32_at(60) as usize;
    if pe > 1024
        || &h[pe..pe + 4] != b"PE\0\0"
        || u16_at(pe + 4) != 0x8664
        || u16_at(pe + 20) < 240
        || u16_at(pe + 24) != 0x20b
    {
        return Err(bad());
    }
    let optional = pe + 24;
    let size = u32_at(optional + 56) as usize;
    let import = u32_at(optional + 120) as usize;
    let length = u32_at(optional + 124) as usize;
    if !(4096..=128 << 20).contains(&size)
        || u32_at(optional + 108) < 2
        || !(40..=65536).contains(&length)
        || import.checked_add(length).is_none_or(|e| e > size)
    {
        return Err(bad());
    }
    let bounded = |at: usize, n: usize| {
        if at.checked_add(n).is_none_or(|e| e > size) {
            Err(bad())
        } else {
            read(at, n)
        }
    };
    let string = |at: usize| -> io::Result<String> {
        let bytes = bounded(at, 128)?;
        let end = bytes.iter().position(|&b| b == 0).ok_or_else(bad)?;
        String::from_utf8(bytes[..end].to_vec()).map_err(|_| bad())
    };
    let mut found = None;
    let mut terminated = false;
    for index in 0..(length / 20).min(128) {
        let d = bounded(import + 20 * index, 20)?;
        if d.iter().all(|&b| b == 0) {
            terminated = true;
            break;
        }
        let n = |o| u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize;
        let original = n(0);
        let name = n(12);
        let iat = n(16);
        if !string(name)?.eq_ignore_ascii_case("kernel32.dll") {
            continue;
        }
        if original == 0 || iat == 0 {
            return Err(bad());
        }
        let mut ended = false;
        for symbol in 0..512 {
            let thunk = bounded(original + 8 * symbol, 8)?;
            let rva = u64::from_le_bytes(thunk.try_into().unwrap());
            if rva == 0 {
                ended = true;
                break;
            }
            if rva & (1 << 63) != 0 {
                continue;
            }
            let rva = usize::try_from(rva).map_err(|_| bad())?;
            if string(rva.checked_add(2).ok_or_else(bad)?)? == "FlushFileBuffers" {
                let slot = iat.checked_add(8 * symbol).ok_or_else(bad)?;
                bounded(slot, 8)?;
                if found.replace(slot).is_some() {
                    return Err(bad());
                }
            }
        }
        if !ended {
            return Err(bad());
        }
    }
    if !terminated {
        return Err(bad());
    }
    found.ok_or_else(bad)
}
#[cfg(windows)]
pub mod windows;
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workload {
    pub child: bool,
    pub sector: u32,
    pub retained: bool,
}
impl Workload {
    pub fn models(self) -> io::Result<(Vec<u8>, Vec<u8>)> {
        if !matches!(self.sector, 512 | 4096) {
            return Err(bad());
        }
        let mut old = vec![if self.child { 7 } else { 0 }; 4 << 20];
        if self.retained {
            old[8195..8203].fill(11);
        }
        let mut new = old.clone();
        let offset = if self.retained { 16387 } else { 8195 };
        new[offset..offset + 8].fill(if self.retained { 13 } else { 11 });
        Ok((old, new))
    }
    pub fn write(self) -> (u64, [u8; 8]) {
        (
            if self.retained { 16387 } else { 8195 },
            [if self.retained { 13 } else { 11 }; 8],
        )
    }
}
#[cfg(windows)]
pub mod runtime;
