//! Shared VMDK descriptor syntax and native profile validation.
use crate::io;
use core::ops::Range;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid VMDK descriptor syntax")
}

pub(crate) fn text(bytes: &[u8]) -> io::Result<&str> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(invalid());
    }
    core::str::from_utf8(&bytes[..end]).map_err(|_| invalid())
}

/// Tracks native singleton fields without allocating or restricting vendor keys.
#[derive(Default)]
pub(crate) struct Properties(u8);
impl Properties {
    /// Returns whether a native field already occurred. Callers choose the
    /// profile-specific error and whether that field is a singleton there.
    pub(crate) fn duplicate(&mut self, key: &str) -> bool {
        let bit = match key {
            "version" => 1,
            "CID" => 2,
            "parentCID" => 4,
            "parentFileNameHint" => 8,
            "createType" => 16,
            _ => return false,
        };
        let duplicate = self.0 & bit != 0;
        self.0 |= bit;
        duplicate
    }
}

pub(crate) struct Field<'a> {
    pub(crate) text: &'a str,
    #[cfg_attr(not(feature = "std"), allow(dead_code))]
    pub(crate) range: Range<usize>,
}

pub(crate) fn property(line: &str, offset: usize) -> Option<(&str, Field<'_>)> {
    let (key, value) = line.split_once('=')?;
    // An extent filename may contain '='. Only a single property token may
    // appear before the assignment operator (with surrounding whitespace).
    if key.trim().is_empty() || key.trim().chars().any(char::is_whitespace) || key.contains('"') {
        return None;
    }
    let start = offset + key.len() + 1 + value.len() - value.trim_start().len();
    let value = value.trim();
    Some((
        key.trim(),
        Field {
            text: value,
            range: start..start + value.len(),
        },
    ))
}

pub(crate) struct Extent<'a> {
    pub(crate) access: &'a str,
    pub(crate) count: Field<'a>,
    pub(crate) kind: &'a str,
    pub(crate) name: &'a str,
    pub(crate) tail: &'a str,
}

pub(crate) fn extent(line: &str, offset: usize) -> io::Result<Extent<'_>> {
    let quote = line.find('"').ok_or_else(invalid)?;
    let close = line[quote + 1..].find('"').ok_or_else(invalid)? + quote + 1;
    let mut fields = line[..quote].split_whitespace();
    let access = fields.next().ok_or_else(invalid)?;
    let count = fields.next().ok_or_else(invalid)?;
    let kind = fields.next().ok_or_else(invalid)?;
    if fields.next().is_some() {
        return Err(invalid());
    }
    let start = count.as_ptr() as usize - line.as_ptr() as usize + offset;
    Ok(Extent {
        access,
        count: Field {
            text: count,
            range: start..start + count.len(),
        },
        kind,
        name: &line[quote + 1..close],
        tail: line[close + 1..].trim(),
    })
}

pub(crate) fn number(value: &str) -> io::Result<u64> {
    value.parse().map_err(|_| invalid())
}

pub(crate) fn sectors(value: &str) -> io::Result<u64> {
    number(value)?.checked_mul(512).ok_or_else(invalid)
}

pub(crate) fn hexadecimal(value: &str) -> io::Result<u32> {
    if value.is_empty() || value.len() > 8 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    u32::from_str_radix(value, 16).map_err(|_| invalid())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Profile {
    MonolithicFlat,
    SplitFlat,
    MonolithicSparse,
    SplitSparse,
}
impl Profile {
    pub(crate) fn sparse(self) -> bool {
        matches!(self, Self::MonolithicSparse | Self::SplitSparse)
    }
    fn monolithic(self) -> bool {
        matches!(self, Self::MonolithicFlat | Self::MonolithicSparse)
    }
}

/// Validated descriptor semantics borrowing the original bytes. Extents are
/// revisited without allocating, and native field spans remain available for
/// writers that patch a descriptor while preserving its other bytes.
pub(crate) struct Descriptor<'a> {
    text: &'a str,
    pub(crate) cid: u32,
    #[cfg_attr(not(feature = "std"), allow(dead_code))]
    pub(crate) cid_range: Range<usize>,
    pub(crate) parent_cid: u32,
    pub(crate) hint: Option<&'a str>,
    pub(crate) profile: Profile,
    pub(crate) sectors: u64,
    pub(crate) extent_count: u64,
}
impl<'a> Descriptor<'a> {
    pub(crate) fn parse(
        text: &'a str,
        mut work: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut properties = Properties::default();
        let mut cid = None;
        let mut cid_range = 0..0;
        let mut parent_cid = None;
        let mut hint = None;
        let mut version = false;
        let mut profile = None;
        let mut sectors = 0u64;
        let mut extent_count = 0u64;
        let mut extent_kind = None;
        let mut offset = 0;
        for original in text.split_inclusive('\n') {
            work()?;
            let line = original.trim();
            if line.is_empty() || line.starts_with('#') {
                offset += original.len();
                continue;
            }
            if let Some((key, field)) = property(original, offset) {
                if properties.duplicate(key) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "duplicate VMDK descriptor property",
                    ));
                }
                match key {
                    "version" if field.text == "1" => version = true,
                    "version" => {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "unsupported VMDK descriptor version",
                        ));
                    }
                    "CID" => {
                        cid = Some(hexadecimal(field.text)?);
                        cid_range = field.range;
                    }
                    "parentCID" => parent_cid = Some(hexadecimal(field.text)?),
                    "parentFileNameHint" => {
                        let name = field
                            .text
                            .strip_prefix('"')
                            .and_then(|s| s.strip_suffix('"'))
                            .ok_or_else(invalid)?;
                        if name.is_empty()
                            || name.contains([':', '\\', '"'])
                            || name.chars().any(char::is_control)
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::Unsupported,
                                "unsupported VMDK parent hint",
                            ));
                        }
                        hint = Some(name);
                    }
                    "createType" => {
                        profile = Some(match field.text {
                            "\"monolithicFlat\"" => Profile::MonolithicFlat,
                            "\"twoGbMaxExtentFlat\"" => Profile::SplitFlat,
                            "\"monolithicSparse\"" => Profile::MonolithicSparse,
                            "\"twoGbMaxExtentSparse\"" => Profile::SplitSparse,
                            _ => {
                                return Err(io::Error::new(
                                    io::ErrorKind::Unsupported,
                                    "unsupported VMDK extent profile",
                                ));
                            }
                        })
                    }
                    key if key.starts_with("ddb.") => {}
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "unsupported VMDK descriptor property",
                        ));
                    }
                }
            } else {
                let extent = extent(original, offset)?;
                if extent.access != "RW" || !matches!(extent.kind, "FLAT" | "SPARSE") {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "unsupported VMDK extent profile",
                    ));
                }
                if extent_kind.is_some_and(|kind| kind != extent.kind) {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "mixed VMDK extent profiles",
                    ));
                }
                extent_kind = Some(extent.kind);
                if extent.name.is_empty()
                    || extent.name.contains([':', '\\'])
                    || extent.name.chars().any(char::is_control)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "unsupported VMDK extent name",
                    ));
                }
                let count = number(extent.count.text)?;
                if count == 0 {
                    return Err(invalid());
                }
                if extent.kind == "FLAT" {
                    self::sectors(extent.tail)?;
                } else if !extent.tail.is_empty() {
                    return Err(invalid());
                }
                sectors = sectors.checked_add(count).ok_or_else(invalid)?;
                sectors.checked_mul(512).ok_or_else(invalid)?;
                extent_count += 1;
            }
            offset += original.len();
        }
        let profile = profile.ok_or_else(invalid)?;
        if !version
            || extent_count == 0
            || (profile.monolithic() && extent_count != 1)
            || profile.sparse() != (extent_kind == Some("SPARSE"))
        {
            return Err(invalid());
        }
        Ok(Self {
            text,
            cid: cid.ok_or_else(invalid)?,
            cid_range,
            parent_cid: parent_cid.ok_or_else(invalid)?,
            hint,
            profile,
            sectors,
            extent_count,
        })
    }

    pub(crate) fn require_parent(&self, allow_parent: bool) -> io::Result<()> {
        if !allow_parent && (self.parent_cid != u32::MAX || self.hint.is_some()) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "VMDK parent requires authorization",
            ));
        }
        Ok(())
    }

    pub(crate) fn extents(&self) -> impl Iterator<Item = io::Result<Extent<'a>>> + '_ {
        let mut offset = 0;
        self.text.split_inclusive('\n').filter_map(move |original| {
            let start = offset;
            offset += original.len();
            let line = original.trim();
            if line.is_empty() || line.starts_with('#') || property(original, start).is_some() {
                None
            } else {
                Some(extent(original, start))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn field_ranges_preserve_descriptor_bytes() {
        let bytes = b"# descriptor\r\n  CID = 012a \r\n RW 128 SPARSE \"disk name.vmdk\"\r\n\0\0";
        let text = text(bytes).unwrap();
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            if let Some((key, value)) = property(line, offset) {
                assert_eq!(key, "CID");
                assert_eq!(&text[value.range], "012a");
                assert_eq!(hexadecimal(value.text).unwrap(), 0x12a);
            } else if line.trim_start().starts_with("RW") {
                let extent = extent(line, offset).unwrap();
                assert_eq!(&text[extent.count.range], "128");
                assert_eq!(extent.name, "disk name.vmdk");
                assert_eq!(sectors(extent.count.text).unwrap(), 65536);
            }
            offset += line.len();
        }
    }
    #[test]
    fn rejects_interior_padding_and_malformed_extent() {
        assert!(text(b"CID=1\0RW 1 SPARSE \"disk\"").is_err());
        assert!(extent("RW 1 SPARSE extra \"disk\"", 0).is_err());
        assert!(extent("RW 1 SPARSE \"disk", 0).is_err());
        assert!(hexadecimal("+1").is_err());
        assert!(sectors("18446744073709551615").is_err());
    }
}
