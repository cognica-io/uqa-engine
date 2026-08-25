//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` COPY text and CSV stream codecs.

use uqa_core::Value;

use super::{CopyFormat, CopyHeader, CopyInputField, CopyOptions};
use crate::{ColumnType, SQLError, SQLResult};

/// Decode a complete text or CSV COPY stream into rows.
pub fn decode_copy_input(
    bytes: &[u8],
    options: &CopyOptions,
    expected_columns: &[String],
) -> Result<Vec<Vec<CopyInputField>>, SQLError> {
    if options.format == CopyFormat::Binary {
        return Err(SQLError::Unsupported(
            "binary COPY format is not implemented".into(),
        ));
    }
    let input = copy_utf8(bytes)?;
    let mut rows = match options.format {
        CopyFormat::Text => decode_text_rows(input, options)?,
        CopyFormat::Csv => decode_csv_rows(input, options)?,
        CopyFormat::Binary => unreachable!(),
    };
    if options.header != CopyHeader::False {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let header = rows.remove(0);
        if options.header == CopyHeader::Match {
            let actual = header
                .into_iter()
                .map(|field| field.unwrap_or_default())
                .collect::<Vec<_>>();
            if actual != expected_columns {
                return Err(copy_data_error(
                    "22P04",
                    format!(
                        "wrong number of fields in header line: expected {expected_columns:?}, got {actual:?}"
                    ),
                    1,
                ));
            }
        }
    }
    let first_data_line = usize::from(options.header != CopyHeader::False) + 1;
    for (index, row) in rows.iter().enumerate() {
        if row.len() > expected_columns.len() {
            return Err(copy_data_error(
                "22P04",
                "extra data after last expected column",
                index + first_data_line,
            ));
        }
        if row.len() < expected_columns.len() {
            return Err(copy_data_error(
                "22P04",
                format!(
                    "missing data for column \"{}\"",
                    expected_columns[row.len()]
                ),
                index + first_data_line,
            ));
        }
    }
    Ok(rows)
}

fn decode_text_rows(
    input: &str,
    options: &CopyOptions,
) -> Result<Vec<Vec<CopyInputField>>, SQLError> {
    let mut rows = Vec::new();
    for (line_index, raw) in logical_text_lines(input).into_iter().enumerate() {
        if raw == "\\." {
            break;
        }
        rows.push(decode_text_row(raw, options, line_index + 1)?);
    }
    Ok(rows)
}

fn decode_text_row(
    raw: &str,
    options: &CopyOptions,
    line: usize,
) -> Result<Vec<CopyInputField>, SQLError> {
    let bytes = raw.as_bytes();
    let mut fields = Vec::new();
    let mut field_start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == options.delimiter {
            fields.push(decode_text_input_field(
                &raw[field_start..index],
                options,
                line,
            )?);
            index += 1;
            field_start = index;
            continue;
        }
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        index += 1;
        if index == bytes.len() {
            return Err(copy_data_error(
                "22P04",
                "unterminated COPY data escape",
                line,
            ));
        }
        match bytes[index] {
            b'x' => {
                index += 1;
                for _ in 0..2 {
                    if index >= bytes.len() || hex_value(bytes[index]).is_none() {
                        break;
                    }
                    index += 1;
                }
            }
            b'0'..=b'7' => {
                index += 1;
                for _ in 0..2 {
                    if index >= bytes.len() || !(b'0'..=b'7').contains(&bytes[index]) {
                        break;
                    }
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    fields.push(decode_text_input_field(&raw[field_start..], options, line)?);
    Ok(fields)
}

fn decode_text_input_field(
    raw: &str,
    options: &CopyOptions,
    line: usize,
) -> Result<CopyInputField, SQLError> {
    if raw == options.null {
        Ok(None)
    } else {
        decode_text_field(raw, line).map(Some)
    }
}

fn logical_text_lines(input: &str) -> Vec<&str> {
    if input.is_empty() {
        return Vec::new();
    }
    let mut lines = input.split('\n').collect::<Vec<_>>();
    if input.ends_with('\n') {
        lines.pop();
    }
    for line in &mut lines {
        if let Some(without_cr) = line.strip_suffix('\r') {
            *line = without_cr;
        }
    }
    lines
}

fn decode_text_field(raw: &str, line: usize) -> Result<String, SQLError> {
    let bytes = raw.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        if index == bytes.len() {
            return Err(copy_data_error(
                "22P04",
                "unterminated COPY data escape",
                line,
            ));
        }
        match bytes[index] {
            b'b' => output.push(0x08),
            b'f' => output.push(0x0c),
            b'n' => output.push(b'\n'),
            b'r' => output.push(b'\r'),
            b't' => output.push(b'\t'),
            b'v' => output.push(0x0b),
            b'x' => {
                let mut value = 0u8;
                let mut digits = 0usize;
                while digits < 2 && index + 1 < bytes.len() {
                    let Some(nibble) = hex_value(bytes[index + 1]) else {
                        break;
                    };
                    value = value * 16 + nibble;
                    index += 1;
                    digits += 1;
                }
                if digits == 0 {
                    output.push(b'x');
                } else {
                    output.push(value);
                }
            }
            digit @ b'0'..=b'7' => {
                let mut value = digit - b'0';
                let mut digits = 1usize;
                while digits < 3 && index + 1 < bytes.len() {
                    let next = bytes[index + 1];
                    if !(b'0'..=b'7').contains(&next) {
                        break;
                    }
                    value = value.wrapping_mul(8).wrapping_add(next - b'0');
                    index += 1;
                    digits += 1;
                }
                output.push(value);
            }
            escaped => output.push(escaped),
        }
        index += 1;
    }
    if output.contains(&0) {
        return Err(copy_encoding_error(Some(line)));
    }
    String::from_utf8(output).map_err(|_| copy_encoding_error(Some(line)))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_csv_rows(
    input: &str,
    options: &CopyOptions,
) -> Result<Vec<Vec<CopyInputField>>, SQLError> {
    let bytes = input.as_bytes();
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = Vec::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut after_quote = false;
    let mut index = 0usize;
    let mut line = 1usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_quotes {
            if byte == options.escape {
                if index + 1 < bytes.len()
                    && matches!(bytes[index + 1], next if next == options.quote || next == options.escape)
                {
                    field.push(bytes[index + 1]);
                    index += 2;
                    continue;
                }
                if options.escape != options.quote {
                    field.push(byte);
                    index += 1;
                    continue;
                }
            }
            if byte == options.quote {
                in_quotes = false;
                after_quote = true;
                index += 1;
                continue;
            }
            if byte == b'\n' {
                line += 1;
            }
            field.push(byte);
            index += 1;
            continue;
        }
        if after_quote && !matches!(byte, b'\n' | b'\r') && byte != options.delimiter {
            field.push(byte);
            index += 1;
            continue;
        }
        if byte == options.delimiter {
            row.push(csv_field(&field, quoted, options)?);
            field.clear();
            quoted = false;
            after_quote = false;
            index += 1;
            continue;
        }
        if byte == b'\n' || byte == b'\r' {
            if byte == b'\r' && index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
                index += 1;
            }
            row.push(csv_field(&field, quoted, options)?);
            if row.len() == 1 && row[0].as_deref() == Some("\\.") && !quoted {
                return Ok(rows);
            }
            rows.push(std::mem::take(&mut row));
            field.clear();
            quoted = false;
            after_quote = false;
            index += 1;
            line += 1;
            continue;
        }
        if byte == options.quote && field.is_empty() && !after_quote {
            quoted = true;
            in_quotes = true;
            index += 1;
            continue;
        }
        field.push(byte);
        index += 1;
    }
    if in_quotes {
        return Err(copy_data_error(
            "22P04",
            "unterminated CSV quoted field",
            line,
        ));
    }
    if !field.is_empty() || !row.is_empty() || quoted || after_quote {
        row.push(csv_field(&field, quoted, options)?);
        if !(row.len() == 1 && row[0].as_deref() == Some("\\.") && !quoted) {
            rows.push(row);
        }
    }
    Ok(rows)
}

fn csv_field(
    field: &[u8],
    quoted: bool,
    options: &CopyOptions,
) -> Result<CopyInputField, SQLError> {
    let text = copy_utf8(field)?;
    if !quoted && text == options.null {
        Ok(None)
    } else {
        Ok(Some(text.to_string()))
    }
}

fn copy_data_error(sqlstate: &str, message: impl Into<String>, line: usize) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("{}\nCONTEXT: COPY data, line {line}", message.into()),
    }
}

fn copy_utf8(bytes: &[u8]) -> Result<&str, SQLError> {
    if bytes.contains(&0) {
        return Err(copy_encoding_error(None));
    }
    std::str::from_utf8(bytes).map_err(|_| copy_encoding_error(None))
}

fn copy_encoding_error(line: Option<usize>) -> SQLError {
    let context = line.map_or_else(String::new, |line| format!(" at line {line}"));
    SQLError::Routine {
        sqlstate: "22021".into(),
        message: format!("invalid byte sequence for encoding \"UTF8\" in COPY data{context}"),
    }
}

/// Encode one SQL result using `PostgreSQL` COPY text or CSV representation.
pub fn encode_copy_result(result: &SQLResult, options: &CopyOptions) -> Result<Vec<u8>, SQLError> {
    if options.format == CopyFormat::Binary {
        return Err(SQLError::Unsupported(
            "binary COPY format is not implemented".into(),
        ));
    }
    let mut output = Vec::new();
    if options.header != CopyHeader::False {
        encode_copy_row(
            result.columns.iter().map(|name| Some(name.as_str())),
            options,
            &mut output,
        );
    }
    for row_index in 0..result.rows.len() {
        let values = result.columns.iter().enumerate().map(|(column, _)| {
            result.value_at(row_index, column).and_then(|value| {
                copy_value_text(
                    value,
                    result.column_types.get(column).and_then(Option::as_ref),
                )
            })
        });
        match options.format {
            CopyFormat::Text => encode_text_value_row(values, options, &mut output),
            CopyFormat::Csv => encode_csv_value_row(values, options, &mut output),
            CopyFormat::Binary => unreachable!(),
        }
    }
    Ok(output)
}

fn copy_value_text(value: &Value, ty: Option<&ColumnType>) -> Option<String> {
    if matches!(value, Value::Null) {
        return None;
    }
    if matches!(ty, Some(ColumnType::Int2Vector | ColumnType::OidVector)) {
        return crate::expr::vector_value_to_string(value);
    }
    Some(match value {
        Value::Bool(true) => "t".into(),
        Value::Bool(false) => "f".into(),
        Value::Float(value) if value.is_nan() => "NaN".into(),
        Value::Float(value) if *value == f64::INFINITY => "Infinity".into(),
        Value::Float(value) if *value == f64::NEG_INFINITY => "-Infinity".into(),
        Value::FixedChar(value) => value.clone(),
        other => crate::expr::value_to_string(other),
    })
}

fn encode_text_value_row(
    fields: impl IntoIterator<Item = Option<String>>,
    options: &CopyOptions,
    output: &mut Vec<u8>,
) {
    for (index, field) in fields.into_iter().enumerate() {
        if index != 0 {
            output.push(options.delimiter);
        }
        match field {
            None => output.extend_from_slice(options.null.as_bytes()),
            Some(field) => encode_text_field(&field, options.delimiter, output),
        }
    }
    output.push(b'\n');
}

fn encode_text_field(field: &str, delimiter: u8, output: &mut Vec<u8>) {
    for byte in field.bytes() {
        match byte {
            0x08 => output.extend_from_slice(b"\\b"),
            0x0c => output.extend_from_slice(b"\\f"),
            b'\n' => output.extend_from_slice(b"\\n"),
            b'\r' => output.extend_from_slice(b"\\r"),
            b'\t' => output.extend_from_slice(b"\\t"),
            0x0b => output.extend_from_slice(b"\\v"),
            b'\\' => output.extend_from_slice(b"\\\\"),
            escaped if escaped == delimiter => {
                output.push(b'\\');
                output.push(escaped);
            }
            other => output.push(other),
        }
    }
}

fn encode_csv_value_row(
    fields: impl IntoIterator<Item = Option<String>>,
    options: &CopyOptions,
    output: &mut Vec<u8>,
) {
    for (index, field) in fields.into_iter().enumerate() {
        if index != 0 {
            output.push(options.delimiter);
        }
        match field {
            None => output.extend_from_slice(options.null.as_bytes()),
            Some(field) => encode_csv_field(&field, options, output),
        }
    }
    output.push(b'\n');
}

fn encode_copy_row<'a>(
    fields: impl IntoIterator<Item = Option<&'a str>>,
    options: &CopyOptions,
    output: &mut Vec<u8>,
) {
    match options.format {
        CopyFormat::Text => encode_text_value_row(
            fields.into_iter().map(|field| field.map(str::to_string)),
            options,
            output,
        ),
        CopyFormat::Csv => encode_csv_value_row(
            fields.into_iter().map(|field| field.map(str::to_string)),
            options,
            output,
        ),
        CopyFormat::Binary => {}
    }
}

fn encode_csv_field(field: &str, options: &CopyOptions, output: &mut Vec<u8>) {
    let needs_quotes = field.is_empty() && options.null.is_empty()
        || field == options.null
        || field.bytes().any(|byte| {
            matches!(byte, b'\n' | b'\r') || byte == options.delimiter || byte == options.quote
        });
    if !needs_quotes {
        output.extend_from_slice(field.as_bytes());
        return;
    }
    output.push(options.quote);
    for byte in field.bytes() {
        if byte == options.quote || byte == options.escape {
            output.push(options.escape);
        }
        output.push(byte);
    }
    output.push(options.quote);
}
