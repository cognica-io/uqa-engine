//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked endian-explicit dictionary primitives.

use super::{DictionaryError, DictionaryResult};

pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    section: &'static str,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], section: &'static str, big_endian: bool) -> Self {
        Self {
            bytes,
            position: 0,
            section,
            big_endian,
        }
    }

    pub fn invalid(&self, reason: &'static str) -> DictionaryError {
        DictionaryError::Invalid {
            section: self.section,
            offset: self.position,
            reason,
        }
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    pub fn take(&mut self, count: usize) -> DictionaryResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| self.invalid("length overflow"))?;
        let result = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| self.invalid("truncated data"))?;
        self.position = end;
        Ok(result)
    }

    pub fn array<const N: usize>(&mut self) -> DictionaryResult<[u8; N]> {
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.take(N)?);
        Ok(bytes)
    }

    pub fn u8(&mut self) -> DictionaryResult<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> DictionaryResult<u16> {
        let bytes = self.array()?;
        Ok(if self.big_endian {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    }

    pub fn u32(&mut self) -> DictionaryResult<u32> {
        let bytes = self.array()?;
        Ok(if self.big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    }

    pub fn u64(&mut self) -> DictionaryResult<u64> {
        let bytes = self.array()?;
        Ok(if self.big_endian {
            u64::from_be_bytes(bytes)
        } else {
            u64::from_le_bytes(bytes)
        })
    }

    pub fn i32(&mut self) -> DictionaryResult<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn var_u32(&mut self) -> DictionaryResult<u32> {
        let mut value = 0;
        for shift in (0..35).step_by(7) {
            let byte = self.u8()?;
            if (shift == 28 && byte > 15) || (shift != 0 && byte == 0) {
                return Err(self.invalid("overflowing or noncanonical variable integer"));
            }
            value |= u32::from(byte & 127) << shift;
            if byte < 128 {
                return Ok(value);
            }
        }
        Err(self.invalid("unterminated variable integer"))
    }

    pub fn count(&mut self, minimum_width: usize) -> DictionaryResult<usize> {
        let count = self.u32()? as usize;
        if count > self.remaining() / minimum_width {
            return Err(self.invalid("record count exceeds remaining bytes"));
        }
        Ok(count)
    }

    pub fn text(&mut self) -> DictionaryResult<&'a str> {
        let length = self.count(1)?;
        Ok(std::str::from_utf8(self.take(length)?)?)
    }

    pub fn finish(self) -> DictionaryResult<()> {
        if self.remaining() != 0 {
            return Err(self.invalid("trailing bytes"));
        }
        Ok(())
    }
}

pub(super) fn vector<T>(capacity: usize) -> DictionaryResult<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity)?;
    Ok(values)
}

#[cfg(any(test, feature = "nori-tools"))]
#[derive(Default)]
pub(super) struct Writer(pub Vec<u8>);

#[cfg(any(test, feature = "nori-tools"))]
impl Writer {
    pub fn bytes(&mut self, bytes: &[u8]) -> DictionaryResult<()> {
        self.0.try_reserve(bytes.len())?;
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    pub fn u8(&mut self, value: u8) -> DictionaryResult<()> {
        self.bytes(&[value])
    }
    pub fn u16(&mut self, value: u16) -> DictionaryResult<()> {
        self.bytes(&value.to_le_bytes())
    }
    pub fn u32(&mut self, value: u32) -> DictionaryResult<()> {
        self.bytes(&value.to_le_bytes())
    }
    pub fn u64(&mut self, value: u64) -> DictionaryResult<()> {
        self.bytes(&value.to_le_bytes())
    }
    pub fn i32(&mut self, value: i32) -> DictionaryResult<()> {
        self.u32(value as u32)
    }

    pub fn var_u32(&mut self, mut value: u32) -> DictionaryResult<()> {
        while value >= 128 {
            self.u8((value as u8 & 127) | 128)?;
            value >>= 7;
        }
        self.u8(value as u8)
    }

    pub fn count(&mut self, count: usize) -> DictionaryResult<()> {
        self.u32(
            u32::try_from(count)
                .map_err(|_| super::error::invalid("encoder", "count exceeds u32"))?,
        )
    }

    pub fn text(&mut self, text: &str) -> DictionaryResult<()> {
        self.count(text.len())?;
        self.bytes(text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::{Reader, Writer};

    #[test]
    fn variable_integers_are_canonical_and_checked_at_every_width() {
        for value in [
            0,
            1,
            127,
            128,
            16383,
            16384,
            0x001f_ffff,
            0x0020_0000,
            0x0fff_ffff,
            0x1000_0000,
            u32::MAX,
        ] {
            let mut writer = Writer::default();
            writer.var_u32(value).unwrap();
            let mut reader = Reader::new(&writer.0, "integer test", false);
            assert_eq!(reader.var_u32().unwrap(), value);
            reader.finish().unwrap();
        }
        for bytes in [
            &[128][..],
            &[128, 0],
            &[255, 255, 255, 255, 16],
            &[128, 128, 128, 128, 128, 1],
        ] {
            assert!(Reader::new(bytes, "integer test", false).var_u32().is_err());
        }
        let mut reader = Reader::new(&[1], "size test", false);
        reader.take(1).unwrap();
        assert!(reader.take(usize::MAX).is_err());
    }
}
