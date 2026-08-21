use crate::diagnostic::{Code, Diagnostic, Family};

pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn read_u8(&mut self) -> Result<u8, Diagnostic> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn read_u16(&mut self) -> Result<u16, Diagnostic> {
        let bytes: [u8; 2] = self.take(2)?.try_into().expect("fixed-width slice");
        Ok(u16::from_le_bytes(bytes))
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32, Diagnostic> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("fixed-width slice");
        Ok(u32::from_le_bytes(bytes))
    }

    pub(crate) fn read_i32(&mut self) -> Result<i32, Diagnostic> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("fixed-width slice");
        Ok(i32::from_le_bytes(bytes))
    }

    pub(crate) fn read_u64(&mut self) -> Result<u64, Diagnostic> {
        let bytes: [u8; 8] = self.take(8)?.try_into().expect("fixed-width slice");
        Ok(u64::from_le_bytes(bytes))
    }

    pub(crate) fn read_uleb32(&mut self) -> Result<u32, Diagnostic> {
        let start = self.position;
        let mut value = 0_u32;

        for group in 0..5 {
            let byte = self.read_u8()?;
            if group == 4 && byte & 0xf0 != 0 {
                return Err(self.error_at(start, Code::IntegerOverflow, "ULEB128 exceeds u32"));
            }
            value |= u32::from(byte & 0x7f) << (group * 7);
            if byte & 0x80 == 0 {
                if group > 0 && byte & 0x7f == 0 {
                    return Err(self.error_at(
                        start,
                        Code::NonCanonicalUleb128,
                        "ULEB128 has a redundant final group",
                    ));
                }
                return Ok(value);
            }
        }

        Err(self.error_at(start, Code::IntegerOverflow, "ULEB128 exceeds five bytes"))
    }

    pub(crate) fn read_utf8(&mut self, length: usize) -> Result<&'a str, Diagnostic> {
        let start = self.position;
        let bytes = self.take(length)?;
        std::str::from_utf8(bytes)
            .map_err(|_| self.error_at(start, Code::InvalidUtf8, "text is not valid UTF-8"))
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], Diagnostic> {
        let start = self.position;
        let end = start.checked_add(length).ok_or_else(|| {
            self.error_at(start, Code::IntegerOverflow, "byte range overflows usize")
        })?;
        let bytes = self.bytes.get(start..end).ok_or_else(|| {
            self.error_at(start, Code::UnexpectedEnd, "value extends beyond input")
        })?;
        self.position = end;
        Ok(bytes)
    }

    fn error_at(&self, offset: usize, code: Code, detail: &'static str) -> Diagnostic {
        Diagnostic::at_offset(Family::Container, code, offset, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_values_and_tracks_offset() {
        let mut cursor = Cursor::new(&[0x34, 0x12, 0x78, 0x56]);
        assert_eq!(cursor.read_u16().unwrap(), 0x1234);
        assert_eq!(cursor.read_u16().unwrap(), 0x5678);
        assert_eq!(cursor.position(), 4);
    }

    #[test]
    fn rejects_non_canonical_uleb128() {
        let error = Cursor::new(&[0x80, 0x00]).read_uleb32().unwrap_err();
        assert_eq!(error.code, crate::diagnostic::Code::NonCanonicalUleb128);
        assert_eq!(error.location.offset, Some(0));
    }

    #[test]
    fn rejects_truncated_fixed_read() {
        let error = Cursor::new(&[1, 2, 3]).read_u32().unwrap_err();
        assert_eq!(error.code, crate::diagnostic::Code::UnexpectedEnd);
    }

    #[test]
    fn reads_signed_wide_and_utf8_values() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(-7_i32).to_le_bytes());
        bytes.extend_from_slice(&9_u64.to_le_bytes());
        bytes.extend_from_slice("ко".as_bytes());
        let mut cursor = Cursor::new(&bytes);

        assert_eq!(cursor.read_i32().unwrap(), -7);
        assert_eq!(cursor.read_u64().unwrap(), 9);
        assert_eq!(cursor.read_utf8("ко".len()).unwrap(), "ко");
    }
}
