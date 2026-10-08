//! CRC-32C for contiguous and circular native VHDX metadata.
pub(crate) fn checksum(bytes: impl IntoIterator<Item = u8>) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    #[test]
    fn castagnoli_standard_check_value_and_empty_input() {
        assert_eq!(super::checksum(b"123456789".iter().copied()), 0xe3069283);
        assert_eq!(super::checksum([]), 0);
    }
}
