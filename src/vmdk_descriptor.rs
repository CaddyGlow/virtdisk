//! Shared VMDK descriptor syntax; supported profiles are checked by callers.
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
