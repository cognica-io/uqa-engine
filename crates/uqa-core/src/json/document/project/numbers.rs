//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generic numeric visitation preserves integers and only canonical finite float spellings.

pub(super) struct Text<'a> {
    original: &'a str,
    formatted: Buffer,
}
impl<'a> Text<'a> {
    fn borrowed(original: &'a str) -> Self {
        Self {
            original,
            formatted: Buffer::new(),
        }
    }
    pub fn as_str(&self) -> &str {
        if self.formatted.len == 0 {
            self.original
        } else {
            self.formatted.as_str()
        }
    }
}

// Every finite f64 fixed or exponential decimal display fits: 309 integer digits,
// or 324 fractional zero positions plus significant digits, sign and punctuation.
pub(super) struct Buffer {
    bytes: [u8; 512],
    len: usize,
}
impl Buffer {
    fn new() -> Self {
        Self {
            bytes: [0; 512],
            len: 0,
        }
    }
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("numeric ASCII")
    }
}
impl std::fmt::Write for Buffer {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(std::fmt::Error)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(std::fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}
impl std::io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = self
            .len
            .checked_add(bytes.len())
            .ok_or(std::io::ErrorKind::InvalidData)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(std::io::ErrorKind::InvalidData)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn project(text: &str) -> Text<'_> {
    if text.len() > 512
        || (text.len() <= 40 && (text.parse::<u128>().is_ok() || text.parse::<i128>().is_ok()))
    {
        return Text::borrowed(text);
    }
    let Ok(value) = text.parse::<f64>() else {
        return Text::borrowed(text);
    };
    if !value.is_finite() {
        return Text::borrowed(text);
    }
    let mut json = Buffer::new();
    serde_json::to_writer(&mut json, &value).expect("finite f64 JSON fits fixed scratch");
    let mut display = Buffer::new();
    std::fmt::write(&mut display, format_args!("{value}"))
        .expect("finite f64 display fits fixed scratch");
    if text == json.as_str() || text == display.as_str() {
        Text {
            original: text,
            formatted: json,
        }
    } else {
        Text::borrowed(text)
    }
}
