//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental COPY output with admitted value formatting and bounded write/cancellation intervals.

use super::{CopyFormat, CopyHeader, CopyOptions};
use crate::{expr::EngineHook, ColumnType, SQLError, SQLResult};
use std::io::{ErrorKind, Write};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

const WRITE_BYTES: usize = 8192;

/// Text/CSV output over an existing sink. The encoder retains no row or encoded-output buffer; only the current value's formatted text can own scratch memory.
pub struct CopyEncoder<'a, 'c, W: Write + ?Sized> {
    options: &'a CopyOptions,
    output: &'a mut W,
    engine: Option<&'a dyn EngineHook>,
    control: ProductionControl<'c>,
}

impl<'a, 'c, W: Write + ?Sized> CopyEncoder<'a, 'c, W> {
    pub fn new(
        options: &'a CopyOptions,
        output: &'a mut W,
        engine: Option<&'a dyn EngineHook>,
        control: ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        if options.format == CopyFormat::Binary {
            return Err(SQLError::Unsupported(
                "binary COPY format is not implemented".into(),
            ));
        }
        Ok(Self {
            options,
            output,
            engine,
            control,
        })
    }

    pub fn write_header(&mut self, columns: &[String]) -> Result<(), SQLError> {
        self.check()?;
        if self.options.header == CopyHeader::False {
            return Ok(());
        }
        for (index, column) in columns.iter().enumerate() {
            if index != 0 {
                self.write(&[self.options.delimiter])?;
            }
            self.field(Some(column), columns.len() == 1)?;
        }
        self.write(b"\n")
    }

    /// Write one positional row, preserving separately valued duplicate column names and a missing value's SQL NULL semantics.
    pub fn write_row<'v>(
        &mut self,
        fields: impl IntoIterator<Item = (Option<&'v Value>, Option<&'v ColumnType>)>,
    ) -> Result<(), SQLError> {
        self.check()?;
        let mut fields = fields.into_iter().peekable();
        let mut first = true;
        while let Some((value, ty)) = fields.next() {
            if !first {
                self.write(&[self.options.delimiter])?;
            }
            let text = value
                .map(|value| value_text(value, ty, self.engine, &self.control))
                .transpose()?
                .flatten();
            self.field(
                text.as_ref().map(FieldText::as_str),
                first && fields.peek().is_none(),
            )?;
            first = false;
        }
        self.write(b"\n")
    }

    /// Finish the sink while the producing statement can still fail and roll back.
    pub fn finish(self) -> Result<(), SQLError> {
        loop {
            self.check()?;
            match self.output.flush() {
                Ok(()) => return self.check(),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(write_error(error)),
            }
        }
    }

    fn check(&self) -> Result<(), SQLError> {
        self.control.check()?;
        crate::expr::check_statement_cancellation()?;
        Ok(())
    }

    fn write(&mut self, mut bytes: &[u8]) -> Result<(), SQLError> {
        while !bytes.is_empty() {
            self.check()?;
            let chunk = &bytes[..bytes.len().min(WRITE_BYTES)];
            match self.output.write(chunk) {
                Ok(0) => return Err(write_error(std::io::Error::from(ErrorKind::WriteZero))),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(write_error(error)),
            }
        }
        Ok(())
    }

    fn field(&mut self, field: Option<&str>, single_column: bool) -> Result<(), SQLError> {
        let Some(field) = field else {
            return self.write(self.options.null.as_bytes());
        };
        match self.options.format {
            CopyFormat::Text => self.text_field(field),
            CopyFormat::Csv => self.csv_field(field, single_column),
            CopyFormat::Binary => unreachable!("constructor rejects binary COPY"),
        }
    }

    fn text_field(&mut self, field: &str) -> Result<(), SQLError> {
        let bytes = field.as_bytes();
        let mut start = 0;
        for (index, byte) in bytes.iter().copied().enumerate() {
            if index % WRITE_BYTES == 0 {
                self.check()?;
            }
            let escaped = match byte {
                0x08 => Some(b'b'),
                0x0c => Some(b'f'),
                b'\n' => Some(b'n'),
                b'\r' => Some(b'r'),
                b'\t' => Some(b't'),
                0x0b => Some(b'v'),
                b'\\' => Some(b'\\'),
                other if other == self.options.delimiter => Some(other),
                _ => None,
            };
            if let Some(escaped) = escaped {
                self.write(&bytes[start..index])?;
                self.write(&[b'\\', escaped])?;
                start = index + 1;
            }
        }
        self.write(&bytes[start..])
    }

    fn csv_field(&mut self, field: &str, single_column: bool) -> Result<(), SQLError> {
        let mut needs_quotes =
            (field.is_empty() && self.options.null.is_empty()) || (single_column && field == r"\.");
        if field.len() == self.options.null.len() {
            let mut same = true;
            for (left, right) in field
                .as_bytes()
                .chunks(WRITE_BYTES)
                .zip(self.options.null.as_bytes().chunks(WRITE_BYTES))
            {
                self.check()?;
                if left != right {
                    same = false;
                    break;
                }
            }
            needs_quotes |= same;
        }
        for chunk in field.as_bytes().chunks(WRITE_BYTES) {
            self.check()?;
            if needs_quotes {
                break;
            }
            needs_quotes = chunk.iter().any(|byte| {
                matches!(byte, b'\n' | b'\r')
                    || *byte == self.options.delimiter
                    || *byte == self.options.quote
            });
        }
        if !needs_quotes {
            return self.write(field.as_bytes());
        }
        self.write(&[self.options.quote])?;
        let bytes = field.as_bytes();
        let mut start = 0;
        for (index, byte) in bytes.iter().copied().enumerate() {
            if index % WRITE_BYTES == 0 {
                self.check()?;
            }
            if byte == self.options.quote || byte == self.options.escape {
                self.write(&bytes[start..index])?;
                self.write(&[self.options.escape, byte])?;
                start = index + 1;
            }
        }
        self.write(&bytes[start..])?;
        self.write(&[self.options.quote])
    }
}

enum FieldText<'v> {
    Borrowed(&'v str),
    Owned(Produced<String>),
}

impl FieldText<'_> {
    fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(text) => text,
            Self::Owned(text) => text,
        }
    }
}

fn value_text<'v>(
    value: &'v Value,
    ty: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<FieldText<'v>>, SQLError> {
    control.check()?;
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let _interval_style = if crate::expr::interval_style::output_can_contain_interval(value) {
        crate::expr::interval_style::live_scope(engine, control)?
    } else {
        None
    };
    Ok(Some(match value {
        Value::Str(text) | Value::Json(text) | Value::JsonB(text) | Value::FixedChar(text) => {
            FieldText::Borrowed(text)
        }
        Value::Bool(value) => FieldText::Borrowed(if *value { "t" } else { "f" }),
        Value::Float(value) if value.is_nan() => FieldText::Borrowed("NaN"),
        Value::Float(value) if *value == f64::INFINITY => FieldText::Borrowed("Infinity"),
        Value::Float(value) if *value == f64::NEG_INFINITY => FieldText::Borrowed("-Infinity"),
        other => {
            let text = if let Some(ty) = ty {
                let catalog = engine.map(crate::expr::EngineValueCatalog);
                crate::result::format_postgres_text_with_catalog_and_control(
                    other,
                    ty,
                    catalog
                        .as_ref()
                        .map(|catalog| catalog as &dyn crate::expr::SQLValueCatalog),
                    control,
                )?
            } else {
                crate::expr::value_to_string_with_control(other, control)?
            };
            FieldText::Owned(text)
        }
    }))
}

fn write_error(error: std::io::Error) -> SQLError {
    SQLError::Routine {
        sqlstate: "58030".into(),
        message: format!("write COPY TO stream: {error}"),
    }
}

/// Write a materialized SQL result without allocating a second, encoded result. Streaming producers use [`CopyEncoder`] directly with one row at a time.
pub fn write_copy_result<W: Write + ?Sized>(
    result: &SQLResult,
    options: &CopyOptions,
    output: &mut W,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut encoder = CopyEncoder::new(options, output, engine, *control)?;
    encoder.write_header(&result.columns)?;
    for row in 0..result.rows.len() {
        encoder.write_row(result.columns.iter().enumerate().map(|(column, _)| {
            (
                result.value_at(row, column),
                result.column_types.get(column).and_then(Option::as_ref),
            )
        }))?;
    }
    encoder.finish()
}

#[cfg(test)]
mod tests;
