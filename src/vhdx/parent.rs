use super::{invalid, u16le, u32le};
use std::{collections::BTreeMap, io};
pub(crate) const ITEM: [u8; 16] = [
    0x2d, 0x5f, 0xd3, 0xa8, 0x0b, 0xb3, 0x4d, 0x45, 0xab, 0xf7, 0xd3, 0xd8, 0x48, 0x34, 0xab, 0x0c,
];
const TYPE: [u8; 16] = [
    0xb7, 0xef, 0x4a, 0xb0, 0x9e, 0xd1, 0x81, 0x4a, 0xb7, 0x89, 0x25, 0xb8, 0xe9, 0x44, 0x59, 0x13,
];
pub(super) struct Locator {
    pub(super) linkage: Vec<[u8; 16]>,
    pub(super) paths: Vec<String>,
}
pub(super) fn parse(bytes: &[u8]) -> io::Result<Locator> {
    if bytes.len() < 20 || bytes[..16] != TYPE || u16le(bytes, 16) != 0 {
        return Err(invalid("invalid VHDX parent locator header"));
    }
    let count = u16le(bytes, 18) as usize;
    let end = 20 + count * 12;
    if count == 0 || end > bytes.len() {
        return Err(invalid("invalid VHDX locator entry count"));
    }
    let mut values = BTreeMap::new();
    let mut extents = Vec::with_capacity(count * 2);
    for e in bytes[20..end].as_chunks::<12>().0 {
        let mut strings = Vec::with_capacity(2);
        for i in 0..2 {
            let start = u32le(e, i * 4) as usize;
            let len = u16le(e, 8 + i * 2) as usize;
            let stop = start
                .checked_add(len)
                .ok_or_else(|| invalid("VHDX locator offset overflow"))?;
            if start < end || len == 0 || !len.is_multiple_of(2) || stop > bytes.len() {
                return Err(invalid("invalid VHDX locator string extent"));
            }
            let words: Vec<u16> = bytes[start..stop]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|w| u16::from_le_bytes(*w))
                .collect();
            let value =
                String::from_utf16(&words).map_err(|_| invalid("invalid VHDX locator UTF-16"))?;
            if value.contains('\0') {
                return Err(invalid("NUL in VHDX locator"));
            }
            extents.push((start, stop));
            strings.push(value);
        }
        let value = strings.pop().unwrap();
        let key = strings.pop().unwrap();
        if values.insert(key, value).is_some() {
            return Err(invalid("duplicate VHDX locator key"));
        }
    }
    extents.sort_unstable();
    if extents.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(invalid("overlapping VHDX locator strings"));
    }
    let primary = values
        .get("parent_linkage")
        .ok_or_else(|| invalid("missing VHDX parent linkage"))?;
    let mut linkage = vec![guid(primary)?];
    if let Some(other) = values.get("parent_linkage2") {
        linkage.push(guid(other)?);
    }
    if let Some(path) = values.get("relative_path")
        && (path.starts_with(['\\', '/']) || path.contains([':', '/']))
    {
        return Err(invalid("invalid native VHDX relative path"));
    }
    if let Some(path) = values.get("volume_path") {
        let tail = path
            .strip_prefix("\\\\?\\Volume")
            .ok_or_else(|| invalid("invalid native VHDX volume path"))?;
        if tail.len() < 40 || tail.as_bytes()[38] != b'\\' {
            return Err(invalid("invalid native VHDX volume path"));
        }
        guid(
            tail.get(..38)
                .ok_or_else(|| invalid("invalid VHDX volume GUID encoding"))?,
        )?;
    }
    if let Some(path) = values.get("absolute_win32_path") {
        let tail = path
            .strip_prefix("\\\\?\\")
            .ok_or_else(|| invalid("invalid native VHDX absolute path"))?;
        let b = tail.as_bytes();
        let drive = b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
        let unc = tail
            .strip_prefix("UNC\\")
            .is_some_and(|s| s.split('\\').filter(|p| !p.is_empty()).count() >= 3);
        if !drive && !unc {
            return Err(invalid("invalid native VHDX absolute path"));
        }
    }
    let paths = ["relative_path", "volume_path", "absolute_win32_path"]
        .iter()
        .filter_map(|k| values.remove(*k))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Err(invalid("missing VHDX parent path"));
    }
    Ok(Locator { linkage, paths })
}
fn guid(s: &str) -> io::Result<[u8; 16]> {
    let b = s.as_bytes();
    if b.len() != 38
        || b[0] != b'{'
        || b[37] != b'}'
        || [9, 14, 19, 24].iter().any(|&i| b[i] != b'-')
    {
        return Err(invalid("invalid VHDX linkage GUID"));
    }
    let hex = b[1..37]
        .iter()
        .copied()
        .filter(|&v| v != b'-')
        .collect::<Vec<_>>();
    let mut result = [0; 16];
    for (i, pair) in hex.as_chunks::<2>().0.iter().enumerate() {
        let a = (pair[0] as char)
            .to_digit(16)
            .ok_or_else(|| invalid("invalid VHDX linkage hex"))?;
        let b = (pair[1] as char)
            .to_digit(16)
            .ok_or_else(|| invalid("invalid VHDX linkage hex"))?;
        result[i] = (a * 16 + b) as u8;
    }
    result[..4].reverse();
    result[4..6].reverse();
    result[6..8].reverse();
    Ok(result)
}
impl super::Vhdx {
    /// Open a clean native chain, authorizing every parent before opening its data.
    /// Sources must remain immutable; chains share limits and cannot repeat file identities.
    pub fn open_chain(
        path: impl AsRef<std::path::Path>,
        authorized_parent_paths: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        Self::open_chain_with_limits(
            path,
            authorized_parent_paths,
            crate::ParserLimits::default(),
        )
    }
    /// Open a parent chain with caller-tightened cumulative limits.
    pub fn open_chain_with_limits(
        path: impl AsRef<std::path::Path>,
        authorized_parent_paths: &[std::path::PathBuf],
        limits: crate::ParserLimits,
    ) -> io::Result<Self> {
        let budget = crate::ReadBudget::new(limits)?;
        let mut approved = std::collections::BTreeMap::new();
        for path in authorized_parent_paths {
            budget.work(1)?;
            budget.metadata((path.as_os_str().as_encoded_bytes().len() as u64 + 64) * 2)?;
            let canonical = std::fs::canonicalize(path)?;
            let identity = same_file::Handle::from_path(&canonical)?;
            approved.insert(canonical, identity);
        }
        let path = std::fs::canonicalize(path)?;
        let mut identities = Vec::new();
        Self::chain_inner(&path, &approved, &mut identities, &budget)
    }
    pub(crate) fn open_locked_chain(
        source: std::sync::Arc<dyn crate::ReadAt>,
        path: &std::path::Path,
        authorized: &[std::path::PathBuf],
        identity: same_file::Handle,
    ) -> io::Result<Self> {
        let path = std::fs::canonicalize(path)?;
        Self::open_chain_source(source, &path, authorized, identity)
    }
    pub(crate) fn open_chain_at(
        path: &std::path::Path,
        locator_directory: &std::path::Path,
        authorized: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        let source = std::sync::Arc::new(crate::RawDisk::open(std::fs::canonicalize(path)?)?);
        let identity = source.identity()?;
        let directory = std::fs::canonicalize(locator_directory)?;
        let context_path = directory.join(
            path.file_name()
                .ok_or_else(|| invalid("VHDX staged source has no filename"))?,
        );
        Self::open_chain_source(source, &context_path, authorized, identity)
    }
    fn open_chain_source(
        source: std::sync::Arc<dyn crate::ReadAt>,
        path: &std::path::Path,
        authorized: &[std::path::PathBuf],
        identity: same_file::Handle,
    ) -> io::Result<Self> {
        let budget = crate::ReadBudget::new(crate::ParserLimits::default())?;
        let mut approved = std::collections::BTreeMap::new();
        for path in authorized {
            budget.work(1)?;
            budget.metadata((path.as_os_str().as_encoded_bytes().len() as u64 + 64) * 2)?;
            let canonical = std::fs::canonicalize(path)?;
            let identity = same_file::Handle::from_path(&canonical)?;
            approved.insert(canonical, identity);
        }
        let image = Self::parse(source, budget.clone(), true)?;
        Self::attach(image, path, &approved, &mut vec![identity], &budget)
    }
    fn chain_inner(
        path: &std::path::Path,
        approved: &std::collections::BTreeMap<std::path::PathBuf, same_file::Handle>,
        identities: &mut Vec<same_file::Handle>,
        budget: &crate::ReadBudget,
    ) -> io::Result<Self> {
        budget.work(1)?;
        if identities.len() as u64 >= budget.limits().recursion_depth.min(32) {
            return Err(invalid("VHDX parent chain depth exceeded"));
        }
        let source = std::sync::Arc::new(crate::RawDisk::open(path)?);
        let identity = source.identity()?;
        if !identities.is_empty() && approved.get(path) != Some(&identity) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "VHDX authorized parent file identity changed",
            ));
        }
        if identities.contains(&identity) {
            return Err(invalid("VHDX parent chain repeats file identity"));
        }
        identities.push(identity);
        let image = Self::parse(source, budget.clone(), true)?;
        Self::attach(image, path, approved, identities, budget)
    }
    fn attach(
        mut image: Self,
        path: &std::path::Path,
        approved: &std::collections::BTreeMap<std::path::PathBuf, same_file::Handle>,
        identities: &mut Vec<same_file::Handle>,
        budget: &crate::ReadBudget,
    ) -> io::Result<Self> {
        if let Some(locator) = &image.locator {
            let mut chosen = None;
            let mut unauthorized = false;
            let mut mismatch = false;
            let mut unsupported_namespace = false;
            for name in &locator.paths {
                budget.work(1)?;
                let candidate = match resolve(path, name) {
                    Ok(p) => p,
                    Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                        unsupported_namespace = true;
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let canonical = match std::fs::canonicalize(candidate) {
                    Ok(p) => p,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                if !approved.contains_key(&canonical) {
                    unauthorized = true;
                    continue;
                }
                let depth = identities.len();
                let parent = Self::chain_inner(&canonical, approved, identities, budget)?;
                if !locator.linkage.contains(&parent.data_guid) {
                    identities.truncate(depth);
                    mismatch = true;
                    continue;
                }
                if image.logical_sector != parent.logical_sector || image.length != parent.length {
                    return Err(invalid("VHDX parent geometry mismatch"));
                }
                chosen = Some(std::sync::Arc::new(parent));
                break;
            }
            image.parent = Some(chosen.ok_or_else(|| {
                if mismatch {
                    invalid("VHDX parent DataWriteGuid linkage mismatch")
                } else if unauthorized {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "VHDX parent path is not explicitly authorized",
                    )
                } else if unsupported_namespace {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "native VHDX parent namespace is unavailable on this host",
                    )
                } else {
                    io::Error::new(io::ErrorKind::NotFound, "VHDX parent path not found")
                }
            })?);
        }
        Ok(image)
    }
}
fn resolve(child: &std::path::Path, name: &str) -> io::Result<std::path::PathBuf> {
    #[cfg(windows)]
    if name.starts_with("\\\\?\\") {
        return Ok(std::path::PathBuf::from(name));
    }
    #[cfg(not(windows))]
    if name.starts_with("\\\\?\\") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows VHDX parent namespace is unavailable",
        ));
    }
    if name.starts_with(['\\', '/']) || name.contains(':') {
        return Err(invalid("unsupported VHDX parent path namespace"));
    }
    Ok(child
        .parent()
        .ok_or_else(|| invalid("VHDX child has no parent directory"))?
        .join(name.replace('\\', "/")))
}

pub(crate) fn encode(linkage: [u8; 16], relative: &str) -> io::Result<Vec<u8>> {
    let mut id = linkage;
    id[..4].reverse();
    id[4..6].reverse();
    id[6..8].reverse();
    let hex: Vec<_> = id.iter().map(|b| format!("{b:02x}")).collect();
    let linkage = format!(
        "{{{}-{}-{}-{}-{}}}",
        hex[..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..].concat()
    );
    let mut b = vec![0; 44];
    b[..16].copy_from_slice(&TYPE);
    b[18] = 2;
    for (i, (key, value)) in [
        ("parent_linkage", linkage.as_str()),
        ("relative_path", relative),
    ]
    .iter()
    .enumerate()
    {
        for (j, s) in [key, value].iter().enumerate() {
            let raw: Vec<_> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let len = u16::try_from(raw.len())
                .map_err(|_| invalid("VHDX locator string exceeds native limit"))?;
            let at = 20 + i * 12;
            let offset = b.len() as u32;
            b[at + j * 4..at + j * 4 + 4].copy_from_slice(&offset.to_le_bytes());
            b[at + 8 + j * 2..at + 10 + j * 2].copy_from_slice(&len.to_le_bytes());
            b.extend(raw);
        }
    }
    Ok(b)
}

impl super::Vhdx {
    /// Open an immutable native recovered child view with explicitly authorized clean parents.
    /// Only the child log is replayed; source files are never modified.
    pub fn open_recovered_chain(
        path: impl AsRef<std::path::Path>,
        authorized_parent_paths: &[std::path::PathBuf],
    ) -> io::Result<Self> {
        Self::open_recovered_chain_with_limits(
            path,
            authorized_parent_paths,
            crate::ParserLimits::default(),
        )
    }
    /// Recover the child view under a budget shared with its authorized parent chain.
    pub fn open_recovered_chain_with_limits(
        path: impl AsRef<std::path::Path>,
        authorized_parent_paths: &[std::path::PathBuf],
        limits: crate::ParserLimits,
    ) -> io::Result<Self> {
        let path = std::fs::canonicalize(path)?;
        let source = std::sync::Arc::new(crate::RawDisk::open(&path)?);
        let identity = source.identity()?;
        Self::recovered_chain_parts(source, &path, authorized_parent_paths, identity, limits)
            .map(|(image, _)| image)
    }
    pub(crate) fn recovered_chain_parts(
        source: std::sync::Arc<dyn crate::ReadAt>,
        path: &std::path::Path,
        authorized: &[std::path::PathBuf],
        identity: same_file::Handle,
        limits: crate::ParserLimits,
    ) -> io::Result<(Self, std::sync::Arc<super::log::Overlay>)> {
        let budget = crate::ReadBudget::new(limits)?;
        let overlay = super::log::recover(source, budget.clone())?;
        let image = Self::parse(overlay.clone(), budget.clone(), true)?;
        overlay.reject_payload_updates(&image.map, image.block)?;
        let mut approved = std::collections::BTreeMap::new();
        for path in authorized {
            budget.work(1)?;
            budget.metadata((path.as_os_str().as_encoded_bytes().len() as u64 + 64) * 2)?;
            let canonical = std::fs::canonicalize(path)?;
            let identity = same_file::Handle::from_path(&canonical)?;
            approved.insert(canonical, identity);
        }
        let path = std::fs::canonicalize(path)?;
        let image = Self::attach(image, &path, &approved, &mut vec![identity], &budget)?;
        Ok((image, overlay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut b = vec![0; 20 + pairs.len() * 12];
        b[..16].copy_from_slice(&TYPE);
        b[18..20].copy_from_slice(&(pairs.len() as u16).to_le_bytes());
        for (i, (key, value)) in pairs.iter().enumerate() {
            for (j, s) in [key, value].iter().enumerate() {
                let start = b.len() as u32;
                let raw: Vec<_> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
                let n = raw.len() as u16;
                let at = 20 + i * 12;
                b[at + j * 4..at + j * 4 + 4].copy_from_slice(&start.to_le_bytes());
                b[at + 8 + j * 2..at + 10 + j * 2].copy_from_slice(&n.to_le_bytes());
                b.extend(raw);
            }
        }
        b
    }
    #[test]
    fn native_guid_and_unicode_path() {
        let _process_boundary = crate::test_sync::writer_test();
        let b = fixture(&[
            ("parent_linkage", "{12345678-1234-5678-9abc-def012345678}"),
            ("relative_path", "..\\親.vhdx"),
        ]);
        let p = parse(&b).unwrap();
        assert_eq!(
            p.linkage[0],
            [
                0x78, 0x56, 0x34, 0x12, 0x34, 0x12, 0x78, 0x56, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34,
                0x56, 0x78
            ]
        );
        assert_eq!(p.paths, ["..\\親.vhdx"]);
    }
    #[test]
    fn malformed_locator_bounds_and_keys() {
        let _process_boundary = crate::test_sync::writer_test();
        let valid = fixture(&[
            ("parent_linkage", "{12345678-1234-5678-9abc-def012345678}"),
            ("relative_path", "parent.vhdx"),
        ]);
        for n in 0..valid.len() {
            assert!(parse(&valid[..n]).is_err(), "truncation {n}");
        }
        for mutation in 0..4 {
            let mut b = valid.clone();
            match mutation {
                0 => b[16] = 1,
                1 => b[20..24].copy_from_slice(&0u32.to_le_bytes()),
                2 => b[28] = 1,
                _ => b[0] ^= 1,
            };
            assert!(parse(&b).is_err());
        }
        assert!(parse(&fixture(&[("relative_path", "p")])).is_err());
        assert!(
            parse(&fixture(&[
                ("parent_linkage", "bad"),
                ("relative_path", "p")
            ]))
            .is_err()
        );
        assert!(
            parse(&fixture(&[
                ("parent_linkage", "{12345678-1234-5678-9abc-def012345678}"),
                ("relative_path", "p"),
                ("relative_path", "q")
            ]))
            .is_err()
        );
    }
    #[test]
    fn native_path_namespaces_and_locator_priority() {
        let _process_boundary = crate::test_sync::writer_test();
        let linkage = "{12345678-1234-5678-9abc-def012345678}";
        let absolute = r"\\?\C:\dir\parent.vhdx";
        let volume = r"\\?\Volume{12345678-1234-5678-9abc-def012345678}\dir\parent.vhdx";
        let p = parse(&fixture(&[
            ("absolute_win32_path", absolute),
            ("parent_linkage", linkage),
            ("relative_path", r"..\parent.vhdx"),
            ("volume_path", volume),
        ]))
        .unwrap();
        assert_eq!(p.paths, [r"..\parent.vhdx", volume, absolute]);
        for (key, bad) in [
            ("relative_path", r"C:\parent.vhdx"),
            ("relative_path", "file://parent.vhdx"),
            ("volume_path", absolute),
            ("absolute_win32_path", r"C:\parent.vhdx"),
        ] {
            assert!(parse(&fixture(&[("parent_linkage", linkage), (key, bad)])).is_err());
        }
        let mut invalid = fixture(&[
            ("parent_linkage", linkage),
            ("relative_path", "parent.vhdx"),
        ]);
        let at = u32le(&invalid, 36) as usize;
        invalid[at..at + 2].copy_from_slice(&0xd800u16.to_le_bytes());
        assert!(parse(&invalid).is_err());
    }
    #[test]
    fn authorized_parent_identity_replacement_is_rejected_before_parsing() {
        let _process_boundary = crate::test_sync::writer_test();
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let root = dir.path().join("root");
        std::fs::write(&parent, b"old").unwrap();
        std::fs::write(&root, b"root").unwrap();
        let approved = std::collections::BTreeMap::from([(
            parent.clone(),
            same_file::Handle::from_path(&parent).unwrap(),
        )]);
        std::fs::rename(&parent, dir.path().join("old-parent")).unwrap();
        std::fs::write(&parent, b"replacement").unwrap();
        let mut identities = vec![same_file::Handle::from_path(root).unwrap()];
        let budget = crate::ReadBudget::new(crate::ParserLimits::default()).unwrap();
        assert_eq!(
            super::super::Vhdx::chain_inner(&parent, &approved, &mut identities, &budget)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    #[test]
    fn staged_child_locator_is_valid_before_and_after_final_directory_publication() {
        let _process_boundary = crate::test_sync::writer_test();
        struct Zero;
        impl crate::ReadAt for Zero {
            fn len(&self) -> u64 {
                1 << 20
            }
            fn read_exact_at(&self, at: u64, out: &mut [u8]) -> io::Result<()> {
                crate::check_range(at, out.len() as u64, 1 << 20)?;
                out.fill(0);
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        std::fs::create_dir(&staging).unwrap();
        let parent = dir.path().join("parent.vhdx");
        let stage = staging.join("child.vhdx");
        let final_path = dir.path().join("child.vhdx");
        crate::create_vhdx(&parent, &Zero).unwrap();
        crate::vhdx_write::create_vhdx_overlay_at(&stage, &parent, &[], dir.path()).unwrap();
        assert!(super::super::Vhdx::open_chain(&stage, std::slice::from_ref(&parent)).is_err());
        let image =
            super::super::Vhdx::open_chain_at(&stage, dir.path(), std::slice::from_ref(&parent))
                .unwrap();
        assert!(image.has_parent());
        assert_eq!(
            image.resolved_parent_path(),
            Some(std::fs::canonicalize(&parent).unwrap())
        );
        std::fs::hard_link(&stage, &final_path).unwrap();
        assert!(super::super::Vhdx::open_chain(&final_path, &[parent]).is_ok());
    }
}
