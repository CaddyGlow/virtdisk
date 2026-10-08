//! Shared JSON string escaping for CLI records.
use std::fmt;

pub(super) struct JsonString<'a>(pub(super) &'a str);
impl fmt::Display for JsonString<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("\"")?;
        for character in self.0.chars() {
            match character {
                '"' => output.write_str("\\\"")?,
                '\\' => output.write_str("\\\\")?,
                '\n' => output.write_str("\\n")?,
                '\r' => output.write_str("\\r")?,
                '\t' => output.write_str("\\t")?,
                value if value < ' ' => write!(output, "\\u{:04x}", value as u32)?,
                value => output.write_str(value.encode_utf8(&mut [0; 4]))?,
            }
        }
        output.write_str("\"")
    }
}

#[cfg(test)]
mod tests {
    use super::JsonString;
    #[test]
    fn escapes_all_controls_quotes_and_backslashes_and_preserves_unicode() {
        assert_eq!(
            JsonString("\"\\\n\r\t\0\u{1f}é😀").to_string(),
            "\"\\\"\\\\\\n\\r\\t\\u0000\\u001fé😀\""
        );
    }
}
