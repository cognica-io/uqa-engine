//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL casts and `PostgreSQL` array-literal parsing.

use super::{
    out_of_range, parse_json, to_decimal, to_f64, typed_json_value, value_to_json, value_to_string,
    Result, SQLError, TemporalValue, Value,
};

/// Cast a value to the named SQL type, mirroring `CAST(expr AS ty)`.
/// Types outside the engine's coercion surface return
/// [`SQLError::Unsupported`]; callers doing best-effort typing (the
/// `PL/pgSQL` interpreter) treat that as "leave the value as-is".
pub fn cast_value(v: &Value, ty: &str) -> Result<Value> {
    cast_value_from(v, ty, None)
}

/// Cast a value while preserving an explicitly declared source type when the
/// runtime carrier erases it. `PostgreSQL` 18 integer/`bytea` casts require this
/// for the source integer's two-, four-, or eight-byte representation.
pub fn cast_value_from(v: &Value, ty: &str, source_ty: Option<&str>) -> Result<Value> {
    if matches!(v, Value::Null) {
        return Ok(Value::Null);
    }
    if let Some(elem_ty) = ty.strip_suffix("[]") {
        // `'{1,2,3}'::int[]` parses the PostgreSQL array literal
        // before casting each element.
        let items: Vec<Value> = match v {
            Value::List(items) => items.clone(),
            Value::Str(s) => parse_pg_array_literal(s)?,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "CAST AS {ty}: expected array, got {other:?}"
                )));
            }
        };
        return items
            .iter()
            .map(|item| cast_value_from(item, elem_ty, None))
            .collect::<Result<Vec<_>>>()
            .map(Value::List);
    }
    let (base, modifier) = split_type_modifier(ty);
    match base {
        "smallint" | "int2" | "pg_catalog.int2" => cast_integer(v, "smallint"),
        "integer" | "int" | "int4" | "serial" | "serial4" | "pg_catalog.int4" => {
            cast_integer(v, "integer")
        }
        "bigint" | "int8" | "bigserial" | "serial8" | "pg_catalog.int8" => {
            cast_integer(v, "bigint")
        }
        "real" | "float4" | "float8" | "double" | "double precision" => {
            Ok(Value::Float(to_f64(v)?))
        }
        "numeric" | "decimal" => {
            let value = to_decimal(v)?;
            if let Some(modifier) = modifier {
                let mut parts = modifier.split(',').map(str::trim);
                let precision: u32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| SQLError::TypeMismatch("bad numeric precision".into()))?;
                let scale: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let rounded = value
                    .round_to_scale(scale)
                    .ok_or_else(|| out_of_range("numeric"))?;
                if !rounded.fits_precision(precision, scale) {
                    return Err(SQLError::Routine {
                        sqlstate: "22003".into(),
                        message: format!(
                            "numeric field overflow: A field with precision {precision}, scale {scale} cannot hold value {}",
                            value.to_sql_string()
                        ),
                    });
                }
                return Ok(Value::Decimal(rounded));
            }
            Ok(Value::Decimal(value))
        }
        "text" | "name" | "uuid" => Ok(Value::Str(value_to_string(v))),
        // varchar(n): an explicit cast truncates to the declared length.
        "varchar" | "character varying" => {
            let text = value_to_string(v);
            let Some(modifier) = modifier else {
                return Ok(Value::Str(text));
            };
            let limit: usize = modifier
                .trim()
                .parse()
                .map_err(|_| SQLError::TypeMismatch(format!("bad length modifier {modifier}")))?;
            Ok(Value::Str(text.chars().take(limit).collect()))
        }
        // bpchar is physically blank-padded. Its implicit text coercion strips
        // those spaces, while a direct result retains them.
        "character" | "char" | "bpchar" => {
            let text = value_to_string(v);
            let limit: usize = match modifier {
                Some(modifier) => modifier.trim().parse().map_err(|_| {
                    SQLError::TypeMismatch(format!("bad length modifier {modifier}"))
                })?,
                None => 1,
            };
            if limit == 0 {
                return Err(SQLError::TypeMismatch(
                    "CHARACTER length must be greater than zero".into(),
                ));
            }
            let mut text = text.chars().take(limit).collect::<String>();
            text.extend(std::iter::repeat_n(
                ' ',
                limit.saturating_sub(text.chars().count()),
            ));
            Ok(Value::FixedChar(text))
        }
        "date" => cast_temporal(v, TemporalValue::parse_date, "date"),
        "time" | "time without time zone" => cast_temporal(v, TemporalValue::parse_time, "time"),
        "timetz" | "time with time zone" => {
            cast_temporal(v, TemporalValue::parse_time_tz, "time with time zone")
        }
        "timestamp" | "datetime" | "timestamp without time zone" => {
            cast_temporal(v, TemporalValue::parse_timestamp, "timestamp")
        }
        "timestamptz" | "timestamp with time zone" => cast_temporal(
            v,
            TemporalValue::parse_timestamp_tz,
            "timestamp with time zone",
        ),
        "interval" => cast_temporal(v, TemporalValue::parse_interval, "interval"),
        "json" => {
            if let Value::Json(text) = v {
                return Ok(Value::Json(text.clone()));
            }
            if let Value::Str(text) | Value::FixedChar(text) = v {
                let _validated = parse_json(text)?;
                return Ok(Value::Json(text.clone()));
            }
            Ok(typed_json_value(&value_to_json(v), false))
        }
        "jsonb" => {
            if let Value::JsonB(text) = v {
                return Ok(Value::JsonB(text.clone()));
            }
            let parsed = match v {
                Value::Json(text) | Value::Str(text) | Value::FixedChar(text) => parse_json(text)?,
                other => value_to_json(other),
            };
            Ok(typed_json_value(&parsed, true))
        }
        "bytea" => match v {
            Value::Bytes(bytes) => Ok(Value::Bytes(bytes.clone())),
            Value::Int(value) => integer_to_bytea(*value, source_ty),
            // PostgreSQL reads `\x...` hex input for bytea.
            Value::Str(s) if s.starts_with("\\x") => {
                let hex = &s[2..];
                let mut out = Vec::with_capacity(hex.len() / 2);
                let bytes = hex.as_bytes();
                let mut i = 0;
                while i + 1 < bytes.len() {
                    let hi = (bytes[i] as char)
                        .to_digit(16)
                        .ok_or_else(|| SQLError::TypeMismatch("invalid hex in bytea".into()))?;
                    let lo = (bytes[i + 1] as char)
                        .to_digit(16)
                        .ok_or_else(|| SQLError::TypeMismatch("invalid hex in bytea".into()))?;
                    out.push((hi * 16 + lo) as u8);
                    i += 2;
                }
                Ok(Value::Bytes(out))
            }
            Value::Str(s) => Ok(Value::Bytes(s.as_bytes().to_vec())),
            other => Ok(Value::Bytes(value_to_string(other).into_bytes())),
        },
        "boolean" | "bool" => cast_boolean(v),
        other => Err(SQLError::Unsupported(format!("CAST AS {other}"))),
    }
}

fn integer_to_bytea(value: i64, source_ty: Option<&str>) -> Result<Value> {
    let source = source_ty
        .map(split_type_modifier)
        .map(|(base, _)| base)
        .unwrap_or("integer");
    let bytes = match source {
        "smallint" | "int2" | "pg_catalog.int2" => i16::try_from(value)
            .map(i16::to_be_bytes)
            .map(|bytes| bytes.to_vec())
            .map_err(|_| out_of_range("smallint"))?,
        "bigint" | "int8" | "bigserial" | "serial8" | "pg_catalog.int8" => {
            value.to_be_bytes().to_vec()
        }
        "integer" | "int" | "int4" | "serial" | "serial4" | "pg_catalog.int4" => {
            i32::try_from(value)
                .map(i32::to_be_bytes)
                .map(|bytes| bytes.to_vec())
                .map_err(|_| out_of_range("integer"))?
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other} to bytea"
            )));
        }
    };
    Ok(Value::Bytes(bytes))
}

/// Split `varchar(10)` / `numeric(10,2)` into `("varchar", Some("10"))`.
pub(super) fn split_type_modifier(ty: &str) -> (&str, Option<&str>) {
    match (ty.find('('), ty.rfind(')')) {
        (Some(open), Some(close)) if close > open => {
            (ty[..open].trim_end(), Some(&ty[open + 1..close]))
        }
        _ => (ty, None),
    }
}

/// CAST to the integer family with `PostgreSQL` conversion rules:
/// float8 rounds half-to-even, numeric rounds half-away-from-zero,
/// strings must be integral text, and the result must fit the target
/// width.
pub(super) fn cast_integer(v: &Value, target: &str) -> Result<Value> {
    let n: i64 = match v {
        Value::Int(n) => *n,
        Value::Bool(b) => i64::from(*b),
        Value::Float(f) => {
            if !f.is_finite() {
                return Err(out_of_range(target));
            }
            let rounded = f.round_ties_even();
            // `i64::MAX as f64` rounds up to 2^63.  Comparing with `>` would
            // therefore admit 2^63 and Rust's float-to-int cast would silently
            // saturate it to `i64::MAX`.
            if rounded < i64::MIN as f64 || rounded >= 9_223_372_036_854_775_808.0 {
                return Err(out_of_range(target));
            }
            rounded as i64
        }
        Value::Decimal(d) => d
            .round_dp(0)
            .to_i64_trunc()
            .ok_or_else(|| out_of_range(target))?,
        Value::Str(s) | Value::FixedChar(s) => {
            s.trim().parse::<i64>().map_err(|_| SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type {target}: \"{s}\""),
            })?
        }
        Value::Bytes(bytes) => bytea_to_integer(bytes, target)?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to {target}"
            )));
        }
    };
    let in_range = match target {
        "smallint" => i16::try_from(n).is_ok(),
        "integer" => i32::try_from(n).is_ok(),
        _ => true,
    };
    if !in_range {
        return Err(out_of_range(target));
    }
    Ok(Value::Int(n))
}

fn bytea_to_integer(bytes: &[u8], target: &str) -> Result<i64> {
    let width = match target {
        "smallint" => 2,
        "integer" => 4,
        _ => 8,
    };
    if bytes.len() > width {
        return Err(out_of_range(target));
    }
    let mut extended = [0_u8; 8];
    let offset = width - bytes.len();
    extended[8 - width + offset..].copy_from_slice(bytes);
    Ok(match width {
        2 => i64::from(i16::from_be_bytes([extended[6], extended[7]])),
        4 => i64::from(i32::from_be_bytes([
            extended[4],
            extended[5],
            extended[6],
            extended[7],
        ])),
        _ => i64::from_be_bytes(extended),
    })
}

/// CAST to boolean: strings follow `PostgreSQL`'s `parse_bool`
/// (prefixes of true/false/yes/no, on/off, 1/0); numbers are non-zero
/// tests.
pub(super) fn cast_boolean(v: &Value) -> Result<Value> {
    match v {
        Value::Bool(b) => Ok(Value::Bool(*b)),
        Value::Int(n) => Ok(Value::Bool(*n != 0)),
        Value::Float(f) => Ok(Value::Bool(*f != 0.0)),
        Value::Decimal(d) => Ok(Value::Bool(!d.is_zero())),
        Value::Str(s) | Value::FixedChar(s) => {
            let text = s.trim().to_ascii_lowercase();
            let matches_prefix = |word: &str| !text.is_empty() && word.starts_with(&text);
            let value = if matches_prefix("true") || matches_prefix("yes") || text == "1" {
                Some(true)
            } else if matches_prefix("false") || matches_prefix("no") || text == "0" {
                Some(false)
            } else if "on" == text {
                Some(true)
            } else if matches_prefix("off") && text.len() >= 2 {
                Some(false)
            } else {
                None
            };
            value.map(Value::Bool).ok_or_else(|| SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type boolean: \"{s}\""),
            })
        }
        other => Err(SQLError::TypeMismatch(format!(
            "cannot cast {other:?} to boolean"
        ))),
    }
}

/// Parse a `PostgreSQL` array literal (`{1,2,3}`, `{"a b",NULL}`,
/// `{{1,2},{3,4}}`) into nested lists of string/NULL values; the caller
/// casts elements.
pub fn parse_pg_array_literal(text: &str) -> Result<Vec<Value>> {
    let mut parser = PgArrayLiteralParser::new(text);
    let items = parser.parse()?;
    if let Err(error) = array_shape(&items) {
        return Err(SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{text}\" ({})", error.message()),
        });
    }
    Ok(items)
}

pub(super) struct PgArrayLiteralParser<'a> {
    source: &'a str,
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> PgArrayLiteralParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            chars: source.chars().peekable(),
        }
    }

    fn parse(&mut self) -> Result<Vec<Value>> {
        self.skip_whitespace();
        let items = self.parse_array()?;
        self.skip_whitespace();
        if self.chars.peek().is_some() {
            return Err(self.error("unexpected content after closing brace"));
        }
        Ok(items)
    }

    fn parse_array(&mut self) -> Result<Vec<Value>> {
        if self.chars.next() != Some('{') {
            return Err(self.error("array value must start with `{`"));
        }
        self.skip_whitespace();
        if self.chars.next_if_eq(&'}').is_some() {
            return Ok(Vec::new());
        }

        let mut items = Vec::new();
        loop {
            self.skip_whitespace();
            items.push(self.parse_element()?);
            self.skip_whitespace();
            match self.chars.next() {
                Some(',') => {
                    self.skip_whitespace();
                    if matches!(self.chars.peek(), None | Some('}')) {
                        return Err(self.error("array contains a missing element"));
                    }
                }
                Some('}') => break,
                Some(_) => {
                    return Err(self.error("array elements must be separated by commas"));
                }
                None => return Err(self.error("array is missing a closing `}`")),
            }
        }
        Ok(items)
    }

    fn parse_element(&mut self) -> Result<Value> {
        match self.chars.peek() {
            Some('{') => self.parse_array().map(Value::List),
            Some('"') => self.parse_quoted_element().map(Value::Str),
            Some(',') | Some('}') | None => Err(self.error("array contains a missing element")),
            Some(_) => self.parse_unquoted_element(),
        }
    }

    fn parse_quoted_element(&mut self) -> Result<String> {
        let _opening_quote = self.chars.next();
        let mut value = String::new();
        loop {
            match self.chars.next() {
                Some('"') => return Ok(value),
                Some('\\') => value.push(
                    self.chars
                        .next()
                        .ok_or_else(|| self.error("quoted element ends with an escape"))?,
                ),
                Some(character) => value.push(character),
                None => return Err(self.error("array contains an unterminated quoted element")),
            }
        }
    }

    fn parse_unquoted_element(&mut self) -> Result<Value> {
        let mut value = String::new();
        while let Some(character) = self.chars.peek().copied() {
            match character {
                ',' | '}' => break,
                '{' | '"' => {
                    return Err(self.error("array contains an unescaped special character"));
                }
                '\\' => {
                    let _escape = self.chars.next();
                    value.push(
                        self.chars
                            .next()
                            .ok_or_else(|| self.error("array element ends with an escape"))?,
                    );
                }
                _ => {
                    let _character = self.chars.next();
                    value.push(character);
                }
            }
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(self.error("array contains a missing element"));
        }
        if value.eq_ignore_ascii_case("null") {
            Ok(Value::Null)
        } else {
            Ok(Value::Str(value.to_string()))
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .chars
            .next_if(|character| character.is_whitespace())
            .is_some()
        {}
    }

    fn error(&self, detail: &str) -> SQLError {
        SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{}\" ({detail})", self.source),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArrayShapeError {
    MixedNesting,
    MismatchedDimensions,
}

impl ArrayShapeError {
    fn message(self) -> &'static str {
        match self {
            Self::MixedNesting => "cannot mix nested arrays and scalar elements",
            Self::MismatchedDimensions => "multidimensional arrays must have matching dimensions",
        }
    }
}

pub(super) fn array_shape(items: &[Value]) -> std::result::Result<Vec<usize>, ArrayShapeError> {
    let mut dimensions = vec![items.len()];
    let mut nested_shape: Option<Vec<usize>> = None;
    let mut has_scalar = false;
    for item in items {
        if let Value::List(nested) = item {
            let shape = array_shape(nested)?;
            if has_scalar {
                return Err(ArrayShapeError::MixedNesting);
            }
            if nested_shape
                .as_ref()
                .is_some_and(|expected| *expected != shape)
            {
                return Err(ArrayShapeError::MismatchedDimensions);
            }
            nested_shape = Some(shape);
        } else {
            if nested_shape.is_some() {
                return Err(ArrayShapeError::MixedNesting);
            }
            has_scalar = true;
        }
    }
    if let Some(shape) = nested_shape {
        dimensions.extend(shape);
    }
    Ok(dimensions)
}

/// Return every dimension of a rectangular array value.
///
/// `PostgreSQL` arrays cannot mix scalar and nested elements or contain
/// sub-arrays with different extents.
pub fn array_dimensions(items: &[Value]) -> Result<Vec<usize>> {
    array_shape(items).map_err(|error| SQLError::TypeMismatch(error.message().to_string()))
}

pub(super) fn cast_temporal(
    v: &Value,
    parse: fn(&str) -> Option<TemporalValue>,
    ty: &str,
) -> Result<Value> {
    match v {
        Value::Temporal(value) => Ok(Value::Temporal(value.clone())),
        other => parse(&value_to_string(other))
            .map(Value::Temporal)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to {ty}"))),
    }
}
