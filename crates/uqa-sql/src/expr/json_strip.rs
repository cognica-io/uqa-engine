//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 signatures and textual JSON handling for null stripping.

use uqa_core::{
    json::{decode_json_string_with_control, JsonReadError},
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    Value,
};

use crate::error::{Result, SQLError};

use super::validate_named_argument_order_with_control;

const PARAMETER_NAMES: [&str; 2] = ["target", "strip_in_arrays"];

/// Map call-order arguments onto the declared `(target, strip_in_arrays DEFAULT false)` slots. `None` means the arity or a named argument does not select either catalogued overload.
pub fn argument_positions(
    name: &str,
    argument_names: &[Option<&str>],
) -> Result<Option<Vec<usize>>> {
    argument_positions_with_control(name, argument_names, &ProductionControl::uncontrolled()).map(
        |positions| {
            positions.map(|positions| {
                positions
                    .into_uncontrolled()
                    .expect("ordinary JSON null-stripping argument positions")
            })
        },
    )
}

pub fn argument_positions_with_control(
    name: &str,
    argument_names: &[Option<&str>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<usize>>>> {
    control.check()?;
    validate_named_argument_order_with_control(argument_names.iter().copied(), control)?;
    let function = name
        .get(..11)
        .filter(|prefix| prefix.eq_ignore_ascii_case("pg_catalog."))
        .map_or(name, |_| &name[11..]);
    if !(function.eq_ignore_ascii_case("json_strip_nulls")
        || function.eq_ignore_ascii_case("jsonb_strip_nulls"))
        || !(1..=2).contains(&argument_names.len())
    {
        return Ok(None);
    }
    let mut occupied = [false; PARAMETER_NAMES.len()];
    let mut positions = ProductionVec::new(*control);
    positions.reserve(argument_names.len())?;
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
        positions.push_copy(position)?;
    }
    Ok(occupied[0].then(|| positions.finish()).transpose()?)
}

pub(super) fn reorder_named_values_with_control(
    function: &str,
    call_args: &[(Option<String>, Value)],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Value>>>> {
    let names = super::call_arguments::evaluated_argument_names_with_control(call_args, control)?;
    let positions = match argument_positions_with_control(function, &names, control) {
        Ok(Some(positions)) => positions,
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Ok(None) | Err(_) => return Ok(None),
    };
    let mut values = [None; PARAMETER_NAMES.len()];
    for ((_, value), position) in call_args.iter().zip(positions.iter().copied()) {
        values[position] = Some(value);
    }
    let default = Value::Bool(false);
    values[1].get_or_insert(&default);
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        let Some(value) = value else { return Ok(None) };
        output.push_produced(control.copy_value(value)?)?;
    }
    Ok(Some(output.finish()?))
}

pub(super) fn invalid_json_input(input: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!("invalid input syntax for type json: \"{input}\""),
    }
}

/// Remove JSON nulls without converting textual `json` through a map-backed value. `PostgreSQL`'s `json` result preserves object order, duplicate keys, and number lexemes while compacting whitespace and decoding JSON string escapes.
#[cfg(test)]
pub(super) fn strip_json_nulls_text(input: &str, strip_in_arrays: bool) -> Result<String> {
    Ok(strip_json_nulls_text_with_control(
        input,
        strip_in_arrays,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary JSON stripping has no lease"))
}

pub(super) fn strip_json_nulls_text_with_control(
    input: &str,
    strip_in_arrays: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut parser = JsonStripParser {
        input,
        position: 0,
        strip_in_arrays,
        control: *control,
    };
    let rendered = parser.parse_value(0)?;
    parser.skip_whitespace()?;
    if parser.position != input.len() {
        return Err(invalid_json_input(input));
    }
    Ok(rendered.text)
}

struct RenderedJson {
    text: Produced<String>,
    is_null: bool,
}

struct JsonStripParser<'a, 'c> {
    input: &'a str,
    position: usize,
    strip_in_arrays: bool,
    control: ProductionControl<'c>,
}

impl JsonStripParser<'_, '_> {
    const MAX_DEPTH: usize = 128;

    fn parse_value(&mut self, depth: usize) -> Result<RenderedJson> {
        self.control.check()?;
        if depth > Self::MAX_DEPTH {
            return Err(invalid_json_input(self.input));
        }
        self.skip_whitespace()?;
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
        self.skip_whitespace()?;
        let mut fields = ProductionString::new(self.control);
        fields.push('{')?;
        let mut emitted = false;
        if self.consume(b'}') {
            fields.push('}')?;
            return Ok(RenderedJson {
                text: fields.finish()?,
                is_null: false,
            });
        }
        loop {
            self.skip_whitespace()?;
            if self.peek() != Some(b'"') {
                return Err(invalid_json_input(self.input));
            }
            let key = self.parse_string()?;
            self.skip_whitespace()?;
            if !self.consume(b':') {
                return Err(invalid_json_input(self.input));
            }
            let value = self.parse_value(depth + 1)?;
            if !value.is_null {
                if emitted {
                    fields.push(',')?;
                }
                fields.push_str(&key)?;
                fields.push(':')?;
                fields.push_str(&value.text)?;
                emitted = true;
            }
            self.skip_whitespace()?;
            if self.consume(b'}') {
                break;
            }
            if !self.consume(b',') {
                return Err(invalid_json_input(self.input));
            }
        }
        fields.push('}')?;
        Ok(RenderedJson {
            text: fields.finish()?,
            is_null: false,
        })
    }

    fn parse_array(&mut self, depth: usize) -> Result<RenderedJson> {
        self.position += 1;
        self.skip_whitespace()?;
        let mut elements = ProductionString::new(self.control);
        elements.push('[')?;
        let mut emitted = false;
        if self.consume(b']') {
            elements.push(']')?;
            return Ok(RenderedJson {
                text: elements.finish()?,
                is_null: false,
            });
        }
        loop {
            let value = self.parse_value(depth + 1)?;
            if !self.strip_in_arrays || !value.is_null {
                if emitted {
                    elements.push(',')?;
                }
                elements.push_str(&value.text)?;
                emitted = true;
            }
            self.skip_whitespace()?;
            if self.consume(b']') {
                break;
            }
            if !self.consume(b',') {
                return Err(invalid_json_input(self.input));
            }
        }
        elements.push(']')?;
        Ok(RenderedJson {
            text: elements.finish()?,
            is_null: false,
        })
    }

    fn parse_string(&mut self) -> Result<Produced<String>> {
        let start = self.position;
        self.position += 1;
        while let Some(byte) = self.peek() {
            self.control.check()?;
            match byte {
                b'"' => {
                    self.position += 1;
                    let source = &self.input[start..self.position];
                    let decoded = decode_json_string_with_control(source.as_bytes(), &self.control)
                        .map_err(|error| match error {
                            JsonReadError::InvalidJson => invalid_json_input(self.input),
                            JsonReadError::Memory(error) => error.into(),
                            JsonReadError::Cancelled(error) => error.into(),
                        })?;
                    return super::json::quote_with_control(&decoded, &self.control);
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
            text: self.control.copy_text(literal)?,
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
                self.consume_digits()?;
            }
            _ => return Err(invalid_json_input(self.input)),
        }
        if self.consume(b'.') {
            let digits = self.position;
            self.consume_digits()?;
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
            self.consume_digits()?;
            if digits == self.position {
                return Err(invalid_json_input(self.input));
            }
        }
        Ok(RenderedJson {
            text: self.control.copy_text(&self.input[start..self.position])?,
            is_null: false,
        })
    }

    fn consume_digits(&mut self) -> Result<()> {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.control.check()?;
            self.position += 1;
        }
        Ok(())
    }

    fn skip_whitespace(&mut self) -> Result<()> {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.control.check()?;
            self.position += 1;
        }
        Ok(())
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
