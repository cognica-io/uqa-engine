//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` array literal parsing, shape validation, and element conversion.

use uqa_core::{ArrayValue, Value};

use crate::error::{Result, SQLError};

use super::cast_value_from;

/// Parse a `PostgreSQL` array literal (`{1,2,3}`, `{"a b",NULL}`,
/// `{{1,2},{3,4}}`) into nested lists of string/NULL values; the caller
/// casts elements.
pub fn parse_pg_array_literal(text: &str) -> Result<ArrayValue> {
    let mut parser = PgArrayLiteralParser::new(text);
    let (declared_dimensions, items) = parser.parse()?;
    if let Err(error) = array_shape(&items) {
        return Err(SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{text}\" ({})", error.message()),
        });
    }
    let array = ArrayValue::try_new(items).ok_or_else(|| SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!("malformed array literal: \"{text}\""),
    })?;
    let Some(declared_dimensions) = declared_dimensions else {
        return Ok(array);
    };
    let declared_lengths = declared_dimensions
        .iter()
        .map(|(_, length)| *length)
        .collect::<Vec<_>>();
    if declared_lengths != array.dimensions() {
        return Err(SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!(
                "malformed array literal: \"{text}\" (specified array dimensions do not match array contents)"
            ),
        });
    }
    let lower_bounds = declared_dimensions
        .into_iter()
        .map(|(lower, _)| lower)
        .collect();
    ArrayValue::with_lower_bounds(array.into_elements(), lower_bounds).ok_or_else(|| {
        SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{text}\""),
        }
    })
}

pub(super) fn cast_array_elements(
    items: &[Value],
    element_type: &str,
    source_element_type: Option<&str>,
) -> Result<Vec<Value>> {
    items
        .iter()
        .map(|item| match item {
            Value::List(nested) => {
                cast_array_elements(nested, element_type, source_element_type).map(Value::List)
            }
            other => cast_value_from(other, element_type, source_element_type),
        })
        .collect()
}

pub(super) struct PgArrayLiteralParser<'a> {
    source: &'a str,
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

type ParsedArrayLiteral = (Option<Vec<(i32, usize)>>, Vec<Value>);

impl<'a> PgArrayLiteralParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            chars: source.chars().peekable(),
        }
    }

    fn parse(&mut self) -> Result<ParsedArrayLiteral> {
        self.skip_whitespace();
        let dimensions = self.parse_dimension_declaration()?;
        let items = self.parse_array()?;
        self.skip_whitespace();
        if self.chars.peek().is_some() {
            return Err(self.error("unexpected content after closing brace"));
        }
        Ok((dimensions, items))
    }

    fn parse_dimension_declaration(&mut self) -> Result<Option<Vec<(i32, usize)>>> {
        if self.chars.peek() != Some(&'[') {
            return Ok(None);
        }
        let mut dimensions = Vec::new();
        while self.chars.next_if_eq(&'[').is_some() {
            self.skip_whitespace();
            let lower = self.parse_dimension_bound()?;
            self.skip_whitespace();
            if self.chars.next() != Some(':') {
                return Err(self.error("array dimension must contain `:`"));
            }
            self.skip_whitespace();
            let upper = self.parse_dimension_bound()?;
            self.skip_whitespace();
            if self.chars.next() != Some(']') {
                return Err(self.error("array dimension is missing a closing `]`"));
            }
            if upper == i32::MAX {
                return Err(SQLError::Routine {
                    sqlstate: "54000".into(),
                    message: format!("array upper bound is too large: {upper}"),
                });
            }
            if upper < lower {
                return Err(SQLError::Routine {
                    sqlstate: "2202E".into(),
                    message: "upper bound cannot be less than lower bound".into(),
                });
            }
            let length = i64::from(upper)
                .checked_sub(i64::from(lower))
                .and_then(|difference| difference.checked_add(1))
                .and_then(|length| usize::try_from(length).ok())
                .ok_or_else(|| self.error("array dimension is out of range"))?;
            dimensions.push((lower, length));
            self.skip_whitespace();
        }
        if self.chars.next() != Some('=') {
            return Err(self.error("array dimensions must be followed by `=`"));
        }
        self.skip_whitespace();
        Ok(Some(dimensions))
    }

    fn parse_dimension_bound(&mut self) -> Result<i32> {
        let mut text = String::new();
        if self
            .chars
            .peek()
            .is_some_and(|character| matches!(character, '+' | '-'))
        {
            text.push(self.chars.next().expect("peeked array bound sign"));
        }
        while self.chars.peek().is_some_and(char::is_ascii_digit) {
            text.push(self.chars.next().expect("peeked array bound digit"));
        }
        if text.is_empty() || matches!(text.as_str(), "+" | "-") {
            return Err(self.error("array dimension bound must be an integer"));
        }
        text.parse()
            .map_err(|_| self.error("array dimension bound is out of range"))
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
        let mut significant_len = 0;
        let mut was_escaped = false;
        while let Some(character) = self.chars.peek().copied() {
            match character {
                ',' | '}' => break,
                '{' | '"' => {
                    return Err(self.error("array contains an unescaped special character"));
                }
                '\\' => {
                    let _escape = self.chars.next();
                    let escaped = self
                        .chars
                        .next()
                        .ok_or_else(|| self.error("array element ends with an escape"))?;
                    value.push(escaped);
                    significant_len = value.len();
                    was_escaped = true;
                }
                _ => {
                    let _character = self.chars.next();
                    value.push(character);
                    if !character.is_whitespace() {
                        significant_len = value.len();
                    }
                }
            }
        }
        value.truncate(significant_len);
        if value.is_empty() {
            return Err(self.error("array contains a missing element"));
        }
        if !was_escaped && value.eq_ignore_ascii_case("null") {
            Ok(Value::Null)
        } else {
            Ok(Value::Str(value))
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
