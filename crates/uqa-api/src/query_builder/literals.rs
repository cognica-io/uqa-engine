//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{SQLError, Value};

pub(super) fn render_value(value: &Value) -> Result<String, SQLError> {
    let rendered = match value {
        Value::Null => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) if f.is_finite() => format!("{f}"),
        Value::Float(f) => {
            return Err(SQLError::TypeMismatch(format!(
                "non-finite filter value `{f}` cannot be rendered as SQL"
            )));
        }
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) | Value::FixedChar(s) => quote_str(s),
        Value::Bytes(bytes) => format!("decode('{}', 'hex')", hex_encode(bytes)?),
        Value::Temporal(t) => quote_str(&t.to_sql_string()),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(render_value).collect::<Result<_, _>>()?;
            format!("ARRAY[{}]", inner.join(", "))
        }
        Value::Map(_) => {
            return Err(SQLError::TypeMismatch(
                "map filter values do not have an unambiguous SQL literal".into(),
            ));
        }
    };
    Ok(rendered)
}

fn hex_encode(bytes: &[u8]) -> Result<String, SQLError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let capacity = bytes
        .len()
        .checked_mul(2)
        .ok_or_else(|| SQLError::TypeMismatch("byte literal length overflow".into()))?;
    let mut encoded = String::new();
    encoded
        .try_reserve_exact(capacity)
        .map_err(|error| SQLError::TypeMismatch(format!("allocate byte literal: {error}")))?;
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

pub(super) fn quote_str(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}
