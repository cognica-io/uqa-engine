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
    let Observation::Float(value) = observe(text) else {
        return Text::borrowed(text);
    };
    let mut json = Buffer::new();
    serde_json::to_writer(&mut json, &value).expect("finite f64 JSON fits fixed scratch");
    Text {
        original: text,
        formatted: json,
    }
}

/// The public generic numeric visitor distinguishes native-width integers, unsupported wide integers, canonical finite floats and arbitrary-precision fallback maps.
pub(in crate::json) enum Observation<'a> {
    Unsigned(u64),
    Signed(i64),
    WideUnsigned(u128),
    WideSigned(i128),
    Float(f64),
    Map(&'a str),
}

pub(in crate::json) fn observe(text: &str) -> Observation<'_> {
    if text.len() <= 20 {
        if let Ok(value) = text.parse::<u64>() {
            return Observation::Unsigned(value);
        }
        if let Ok(value) = text.parse::<i64>() {
            return Observation::Signed(value);
        }
    }
    if text.len() <= 40 {
        if let Ok(value) = text.parse::<u128>() {
            return Observation::WideUnsigned(value);
        }
        if let Ok(value) = text.parse::<i128>() {
            return Observation::WideSigned(value);
        }
    }
    if text.len() <= 512 {
        if let Ok(value) = text.parse::<f64>() {
            if value.is_finite() {
                let mut json = Buffer::new();
                serde_json::to_writer(&mut json, &value)
                    .expect("finite f64 JSON fits fixed scratch");
                let mut display = Buffer::new();
                std::fmt::write(&mut display, format_args!("{value}"))
                    .expect("finite f64 display fits fixed scratch");
                if text == json.as_str() || text == display.as_str() {
                    return Observation::Float(value);
                }
            }
        }
    }
    Observation::Map(text)
}

pub(super) async fn buffered<W: crate::json::JsonReadWork<Error = crate::json::JsonReadError>>(
    output: &mut super::JsonDocument,
    text: &str,
    control: &crate::memory::ProductionControl<'_>,
    work: &mut W,
    protocol: crate::json::JsonScalarProtocol,
) -> super::Result<usize> {
    use super::{copy_text, Value};
    let mut formatted = Buffer::new();
    match observe(text) {
        Observation::Unsigned(value) => std::fmt::write(&mut formatted, format_args!("{value}"))
            .expect("u64 fits numeric scratch"),
        Observation::Signed(value) => std::fmt::write(&mut formatted, format_args!("{value}"))
            .expect("i64 fits numeric scratch"),
        Observation::Float(value) => serde_json::to_writer(&mut formatted, &value)
            .expect("finite f64 JSON fits numeric scratch"),
        Observation::WideUnsigned(value) => {
            return Err(crate::json::JsonDiagnostic::wide_unsigned(value, "any value").into())
        }
        Observation::WideSigned(value) => {
            return Err(crate::json::JsonDiagnostic::wide_signed(value, "any value").into())
        }
        Observation::Map(text) => {
            let text = copy_text(text, control, work).await?;
            let value = output.insert(Value::String(text))?;
            let name = copy_text(protocol.number, control, work).await?;
            let mut fields = crate::memory::OwnedMap::new();
            fields
                .insert_admitted_key_from(
                    name,
                    value,
                    &mut super::read::Comparison { control, work },
                )
                .await?;
            return output.insert(Value::Object(fields));
        }
    }
    output.insert(Value::Number(
        copy_text(formatted.as_str(), control, work).await?,
    ))
}
