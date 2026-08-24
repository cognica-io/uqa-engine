//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 signatures and textual JSON handling for null stripping.

use uqa_core::Value;

use crate::error::{Result, SQLError};

use super::validate_named_argument_order;

const PARAMETER_NAMES: [&str; 2] = ["target", "strip_in_arrays"];

/// Map call-order arguments onto the declared `(target, strip_in_arrays DEFAULT false)` slots. `None` means the arity or a named argument does not select either catalogued overload.
pub fn argument_positions(
    name: &str,
    argument_names: &[Option<&str>],
) -> Result<Option<Vec<usize>>> {
    validate_named_argument_order(argument_names.iter().copied())?;
    let lower = name.to_ascii_lowercase();
    let function = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if !matches!(function, "json_strip_nulls" | "jsonb_strip_nulls")
        || !(1..=2).contains(&argument_names.len())
    {
        return Ok(None);
    }
    let mut occupied = [false; PARAMETER_NAMES.len()];
    let mut positions = Vec::with_capacity(argument_names.len());
    let mut positional = 0usize;
    for argument_name in argument_names {
        let position = if let Some(argument_name) = argument_name {
            PARAMETER_NAMES
                .iter()
                .position(|candidate| candidate == argument_name)
        } else {
            let position = positional;
            positional += 1;
            Some(position)
        };
        let Some(position) = position.filter(|position| *position < occupied.len()) else {
            return Ok(None);
        };
        if occupied[position] {
            return Ok(None);
        }
        occupied[position] = true;
        positions.push(position);
    }
    Ok(occupied[0].then_some(positions))
}

pub(super) fn reorder_named_values(
    function: &str,
    call_args: &[(Option<String>, Value)],
) -> Option<Vec<Value>> {
    let argument_names = call_args
        .iter()
        .map(|(name, _)| name.as_deref())
        .collect::<Vec<_>>();
    let positions = argument_positions(function, &argument_names)
        .ok()
        .flatten()?;
    let mut values = vec![None; PARAMETER_NAMES.len()];
    for ((_, value), position) in call_args.iter().zip(positions) {
        values[position] = Some(value.clone());
    }
    values[1].get_or_insert(Value::Bool(false));
    values.into_iter().collect()
}

pub(super) fn invalid_json_input(input: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!("invalid input syntax for type json: \"{input}\""),
    }
}

/// Remove JSON nulls without converting textual `json` through a map-backed value. `PostgreSQL`'s `json` result preserves object order, duplicate keys, and number lexemes while compacting whitespace and decoding JSON string escapes.
pub(super) fn strip_json_nulls_text(input: &str, strip_in_arrays: bool) -> Result<String> {
    let mut parser = JsonStripParser {
        input,
        position: 0,
        strip_in_arrays,
    };
    let rendered = parser.parse_value(0)?;
    parser.skip_whitespace();
    if parser.position != input.len() {
        return Err(invalid_json_input(input));
    }
    Ok(rendered.text)
}

struct RenderedJson {
    text: String,
    is_null: bool,
}

struct JsonStripParser<'a> {
    input: &'a str,
    position: usize,
    strip_in_arrays: bool,
}

impl JsonStripParser<'_> {
    const MAX_DEPTH: usize = 128;

    fn parse_value(&mut self, depth: usize) -> Result<RenderedJson> {
        if depth > Self::MAX_DEPTH {
            return Err(invalid_json_input(self.input));
        }
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.parse_object(depth),
            Some(b'[') => self.parse_array(depth),
            Some(b'"') => self.parse_string().map(|text| RenderedJson {
                text,
                is_null: false,
            }),
            Some(b't') => self.parse_literal("true", false),
            Some(b'f') => self.parse_literal("false", false),
            Some(b'n') => self.parse_literal("null", true),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            _ => Err(invalid_json_input(self.input)),
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<RenderedJson> {
        self.position += 1;
        self.skip_whitespace();
        let mut fields = Vec::new();
        if self.consume(b'}') {
            return Ok(RenderedJson {
                text: "{}".into(),
                is_null: false,
            });
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(invalid_json_input(self.input));
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            if !self.consume(b':') {
                return Err(invalid_json_input(self.input));
            }
            let value = self.parse_value(depth + 1)?;
            if !value.is_null {
                fields.push(format!("{key}:{}", value.text));
            }
            self.skip_whitespace();
            if self.consume(b'}') {
                break;
            }
            if !self.consume(b',') {
                return Err(invalid_json_input(self.input));
            }
        }
        Ok(RenderedJson {
            text: format!("{{{}}}", fields.join(",")),
            is_null: false,
        })
    }

    fn parse_array(&mut self, depth: usize) -> Result<RenderedJson> {
        self.position += 1;
        self.skip_whitespace();
        let mut elements = Vec::new();
        if self.consume(b']') {
            return Ok(RenderedJson {
                text: "[]".into(),
                is_null: false,
            });
        }
        loop {
            let value = self.parse_value(depth + 1)?;
            if !self.strip_in_arrays || !value.is_null {
                elements.push(value.text);
            }
            self.skip_whitespace();
            if self.consume(b']') {
                break;
            }
            if !self.consume(b',') {
                return Err(invalid_json_input(self.input));
            }
        }
        Ok(RenderedJson {
            text: format!("[{}]", elements.join(",")),
            is_null: false,
        })
    }

    fn parse_string(&mut self) -> Result<String> {
        let start = self.position;
        self.position += 1;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.position += 1;
                    let source = &self.input[start..self.position];
                    let decoded = serde_json::from_str::<String>(source)
                        .map_err(|_| invalid_json_input(self.input))?;
                    return serde_json::to_string(&decoded)
                        .map_err(|_| invalid_json_input(self.input));
                }
                b'\\' => {
                    self.position += 1;
                    if self.peek().is_none() {
                        return Err(invalid_json_input(self.input));
                    }
                    self.position += 1;
                }
                _ => self.position += 1,
            }
        }
        Err(invalid_json_input(self.input))
    }

    fn parse_literal(&mut self, literal: &str, is_null: bool) -> Result<RenderedJson> {
        if !self.input[self.position..].starts_with(literal) {
            return Err(invalid_json_input(self.input));
        }
        self.position += literal.len();
        Ok(RenderedJson {
            text: literal.into(),
            is_null,
        })
    }

    fn parse_number(&mut self) -> Result<RenderedJson> {
        let start = self.position;
        self.consume(b'-');
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => {
                self.position += 1;
                self.consume_digits();
            }
            _ => return Err(invalid_json_input(self.input)),
        }
        if self.consume(b'.') {
            let digits = self.position;
            self.consume_digits();
            if digits == self.position {
                return Err(invalid_json_input(self.input));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            let digits = self.position;
            self.consume_digits();
            if digits == self.position {
                return Err(invalid_json_input(self.input));
            }
        }
        Ok(RenderedJson {
            text: self.input[start..self.position].into(),
            is_null: false,
        })
    }

    fn consume_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.position += 1;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.position).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::{argument_positions, strip_json_nulls_text};

    #[test]
    fn json_strip_positions_accept_the_default_and_declaration_order_names() {
        assert_eq!(
            argument_positions("json_strip_nulls", &[None]).unwrap(),
            Some(vec![0])
        );
        assert_eq!(
            argument_positions(
                "jsonb_strip_nulls",
                &[Some("strip_in_arrays"), Some("target")]
            )
            .unwrap(),
            Some(vec![1, 0])
        );
        assert_eq!(
            argument_positions("json_strip_nulls", &[Some("strip_in_arrays")]).unwrap(),
            None
        );
        assert_eq!(
            argument_positions("json_strip_nulls", &[Some("unknown"), Some("target")]).unwrap(),
            None
        );
    }

    #[test]
    fn textual_json_null_stripping_preserves_order_duplicates_and_number_lexemes() {
        let input = r#" { "z" : 1.2300e+02, "a" : null, "z" : 2, "s" : "\u0061", "nested" : [null,{"drop":null,"keep":3}] } "#;
        assert_eq!(
            strip_json_nulls_text(input, false).unwrap(),
            r#"{"z":1.2300e+02,"z":2,"s":"a","nested":[null,{"keep":3}]}"#
        );
        assert_eq!(
            strip_json_nulls_text(input, true).unwrap(),
            r#"{"z":1.2300e+02,"z":2,"s":"a","nested":[{"keep":3}]}"#
        );
        assert_eq!(strip_json_nulls_text("null", true).unwrap(), "null");
    }

    #[test]
    fn textual_json_null_stripping_rejects_malformed_input_with_json_sqlstate() {
        for input in [r#"{"a":}"#, r#"{"a":01}"#, r"[1,]", r#""\uD800""#] {
            assert_eq!(
                strip_json_nulls_text(input, false).unwrap_err().sqlstate(),
                Some("22P02")
            );
        }
    }
}
