use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{self, Read},
    path::{Component, Path},
};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub path: String,
    pub length: u64,
    pub sha256: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Clean,
    NativeReplay,
    LibraryRecovery,
}
/// Minimal native provider rights, independent from surfaced device access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeOpenPolicy {
    pub access_mask: i32,
    pub rw_depth: u32,
    pub attach_flags: i32,
}
/// Only native redo replay opens the disposable leaf backing store writable.
/// Every mode still attaches a read-only device without drive letters.
pub fn native_open_policy(mode: Mode) -> NativeOpenPolicy {
    let replay = mode == Mode::NativeReplay;
    NativeOpenPolicy {
        access_mask: 0x000d0000 | if replay { 0x00020000 } else { 0 },
        rw_depth: u32::from(replay),
        attach_flags: 1 | 2,
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub image: String,
    pub parents: Vec<String>,
    pub files: Vec<FileIdentity>,
    pub capacity: u64,
    pub logical_sector: u32,
    pub physical_sector: u32,
    pub logical_sha256: String,
    pub mode: Mode,
}
fn bad(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn path_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains(['\\', ':'])
        && Path::new(value)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}
fn hash_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = [0; 65536];
    let mut hash = Sha256::new();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn validate(root: &Path, m: &Manifest) -> io::Result<()> {
    if m.version != 1
        || m.files.is_empty()
        || m.files.len() > 32
        || m.parents.len() > 16
        || m.capacity == 0
        || m.capacity > 64 * 1024 * 1024 * 1024
        || !matches!(m.logical_sector, 512 | 4096)
        || !matches!(m.physical_sector, 512 | 4096)
        || !m.capacity.is_multiple_of(u64::from(m.logical_sector))
        || !hash_valid(&m.logical_sha256)
    {
        return Err(bad("manifest limits/geometry/hash invalid"));
    }
    let mut names = HashSet::new();
    for f in &m.files {
        if !path_valid(&f.path)
            || !hash_valid(&f.sha256)
            || f.length > 128 * 1024 * 1024 * 1024
            || !names.insert(&f.path)
        {
            return Err(bad("invalid or duplicate manifest identity"));
        }
    }
    let mut parents = HashSet::new();
    if !path_valid(&m.image)
        || !names.contains(&m.image)
        || m.parents
            .iter()
            .any(|p| !path_valid(p) || p == &m.image || !names.contains(p) || !parents.insert(p))
        || names.len() != m.parents.len() + 1
    {
        return Err(bad(
            "all files must be image or explicitly authorized parent",
        ));
    }
    for f in &m.files {
        let mut path = root.to_path_buf();
        for c in Path::new(&f.path).components() {
            path.push(c);
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(bad("symlink/reparse image trees forbidden"));
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(bad("reparse point forbidden"));
                }
            }
        }
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_file() || metadata.len() != f.length || hash_file(&path)? != f.sha256 {
            return Err(bad("manifest file identity mismatch"));
        }
    }
    Ok(())
}
pub fn copy_tree(root: &Path, m: &Manifest) -> io::Result<tempfile::TempDir> {
    validate(root, m)?;
    let copy = tempfile::Builder::new()
        .prefix("virtdisk-native-acceptance-")
        .tempdir()?;
    for f in &m.files {
        let out = copy.path().join(&f.path);
        std::fs::create_dir_all(out.parent().unwrap())?;
        std::fs::copy(root.join(&f.path), out)?;
    }
    validate(copy.path(), m)?;
    Ok(copy)
}
pub fn authorized_view(root: &Path, m: &Manifest) -> io::Result<virtdisk::Vhdx> {
    let parents: Vec<_> = m.parents.iter().map(|p| root.join(p)).collect();
    match m.mode {
        Mode::Clean => Ok(virtdisk::Vhdx::open_chain(root.join(&m.image), &parents)?),
        _ => Ok(virtdisk::Vhdx::open_recovered_chain(
            root.join(&m.image),
            &parents,
        )?),
    }
}
pub fn validate_locator_keys(entries: &[(String, String)]) -> io::Result<()> {
    let mut keys = HashSet::new();
    for (key, value) in entries {
        if !keys.insert(key)
            || !matches!(
                key.as_str(),
                "parent_linkage" | "parent_linkage2" | "relative_path"
            )
            || value.contains('\0')
        {
            return Err(bad(
                "native acceptance permits only linkage and one relative locator; alternative host paths forbidden",
            ));
        }
    }
    if !keys.contains(&"relative_path".to_owned()) || !keys.contains(&"parent_linkage".to_owned()) {
        return Err(bad("native child locator incomplete"));
    }
    Ok(())
}
pub fn strict_locators(root: &Path, m: &Manifest) -> io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let meta_guid = [
        6, 0xa2, 0x7c, 0x8b, 0x90, 0x47, 0x9a, 0x4b, 0xb8, 0xfe, 0x57, 0x5f, 5, 0xf, 0x88, 0x6e,
    ];
    let locator_guid = [
        0x2d, 0x5f, 0xd3, 0xa8, 0x0b, 0xb3, 0x4d, 0x45, 0xab, 0xf7, 0xd3, 0xd8, 0x48, 0x34, 0xab,
        0x0c,
    ];
    let canonical = root.canonicalize()?;
    let authorized: HashSet<_> = m
        .parents
        .iter()
        .map(|p| root.join(p).canonicalize())
        .collect::<io::Result<_>>()?;
    for identity in &m.files {
        let path = root.join(&identity.path);
        let mut file = std::fs::File::open(&path)?;
        let mut table = [0; 4096];
        file.seek(SeekFrom::Start(196608))?;
        file.read_exact(&mut table)?;
        let count = u32::from_le_bytes(table[8..12].try_into().unwrap()) as usize;
        if count > 126 || &table[..4] != b"regi" {
            return Err(bad("unsupported region table for native gate"));
        }
        let entry = table[16..16 + count * 32]
            .as_chunks::<32>()
            .0
            .iter()
            .find(|e| e[..16] == meta_guid)
            .ok_or_else(|| bad("no metadata region"))?;
        let offset = u64::from_le_bytes(entry[16..24].try_into().unwrap());
        let length = u32::from_le_bytes(entry[24..28].try_into().unwrap()) as usize;
        if !(65536..=1 << 20).contains(&length) {
            return Err(bad("native fixture metadata bound"));
        }
        let mut metadata = vec![0; length];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut metadata)?;
        let count = u16::from_le_bytes(metadata[10..12].try_into().unwrap()) as usize;
        if count > 2047 {
            return Err(bad("native fixture metadata count"));
        }
        for item in metadata[32..32 + count * 32].as_chunks::<32>().0 {
            if item[..16] != locator_guid {
                continue;
            }
            let start = u32::from_le_bytes(item[16..20].try_into().unwrap()) as usize;
            let length = u32::from_le_bytes(item[20..24].try_into().unwrap()) as usize;
            let body = metadata
                .get(
                    start
                        ..start
                            .checked_add(length)
                            .ok_or_else(|| bad("locator overflow"))?,
                )
                .ok_or_else(|| bad("locator outside metadata"))?;
            if body.len() < 20 {
                return Err(bad("locator short"));
            }
            let count = u16::from_le_bytes(body[18..20].try_into().unwrap()) as usize;
            if count > 16 || 20 + count * 12 > body.len() {
                return Err(bad("locator count"));
            }
            let mut entries = Vec::new();
            for descriptor in body[20..20 + count * 12].as_chunks::<12>().0 {
                let mut values = Vec::new();
                for index in 0..2 {
                    let start = u32::from_le_bytes(
                        descriptor[index * 4..index * 4 + 4].try_into().unwrap(),
                    ) as usize;
                    let length = u16::from_le_bytes(
                        descriptor[8 + index * 2..10 + index * 2]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    if !length.is_multiple_of(2) {
                        return Err(bad("locator utf16 alignment"));
                    }
                    let slice = body
                        .get(
                            start
                                ..start
                                    .checked_add(length)
                                    .ok_or_else(|| bad("locator string overflow"))?,
                        )
                        .ok_or_else(|| bad("locator string bounds"))?;
                    values.push(
                        String::from_utf16(
                            &slice
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|w| u16::from_le_bytes(*w))
                                .collect::<Vec<_>>(),
                        )
                        .map_err(|_| bad("locator utf16"))?,
                    );
                }
                entries.push((values.remove(0), values.remove(0)));
            }
            validate_locator_keys(&entries)?;
            let relative = &entries.iter().find(|e| e.0 == "relative_path").unwrap().1;
            if relative.starts_with(['\\', '/']) || relative.contains([':', '/']) {
                return Err(bad("unsafe native locator"));
            }
            let target = path
                .parent()
                .unwrap()
                .join(relative.replace('\\', "/"))
                .canonicalize()?;
            if !target.starts_with(&canonical) || !authorized.contains(&target) {
                return Err(bad("native locator escapes authorized copied tree"));
            }
        }
    }
    Ok(())
}
#[cfg(windows)]
pub mod native;

/// Fixed native production workload on a manifest-pinned 4 MiB standalone model.
pub fn producer_plan(m: &Manifest) -> io::Result<Vec<(u64, Vec<u8>)>> {
    if m.mode != Mode::Clean
        || !m.parents.is_empty()
        || m.files.len() != 1
        || m.capacity != 4 << 20
        || !matches!(m.logical_sector, 512 | 4096)
        || m.logical_sha256 != format!("{:x}", Sha256::digest(vec![7u8; 4 << 20]))
    {
        return Err(bad(
            "producer requires clean standalone constant-seven 4 MiB model",
        ));
    }
    Ok([(8192, 11), ((1 << 20) + 8192, 13), ((3 << 20) + 8192, 17)]
        .into_iter()
        .map(|(offset, value)| (offset, vec![value; m.logical_sector as usize]))
        .collect())
}

pub mod mutation;
pub mod process_gate;
