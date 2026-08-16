//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` `jsonb` structural equality, ordering, and canonical hash keys.

use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
struct JsonNumber {
    negative: bool,
    digits: Vec<u8>,
    power: i64,
}

impl JsonNumber {
    fn parse(text: &str) -> Option<Self> {
        let (negative, unsigned) = text
            .strip_prefix('-')
            .map_or((false, text), |unsigned| (true, unsigned));
        let exponent = unsigned
            .split_once('e')
            .or_else(|| unsigned.split_once('E'));
        let (mantissa, exponent) = match exponent {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
            None => (unsigned, 0_i64),
        };
        let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let mut digits = integer
            .bytes()
            .chain(fraction.bytes())
            .skip_while(|digit| *digit == b'0')
            .collect::<Vec<_>>();
        if digits.is_empty() {
            return Some(Self {
                negative: false,
                digits: vec![b'0'],
                power: 0,
            });
        }
        let fraction_len = i64::try_from(fraction.len()).ok()?;
        let mut power = exponent.checked_sub(fraction_len)?;
        while digits.len() > 1 && digits.last() == Some(&b'0') {
            digits.pop();
            power = power.checked_add(1)?;
        }
        Some(Self {
            negative,
            digits,
            power,
        })
    }

    fn cmp(&self, other: &Self) -> Ordering {
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let magnitude = self.cmp_magnitude(other);
        if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }

    fn cmp_magnitude(&self, other: &Self) -> Ordering {
        let left_integer_digits = i128::try_from(self.digits.len())
            .unwrap_or(i128::MAX)
            .saturating_add(i128::from(self.power));
        let right_integer_digits = i128::try_from(other.digits.len())
            .unwrap_or(i128::MAX)
            .saturating_add(i128::from(other.power));
        let ordering = left_integer_digits.cmp(&right_integer_digits);
        if ordering != Ordering::Equal {
            return ordering;
        }
        let width = self.digits.len().max(other.digits.len());
        (0..width)
            .map(|index| {
                self.digits
                    .get(index)
                    .copied()
                    .unwrap_or(b'0')
                    .cmp(&other.digits.get(index).copied().unwrap_or(b'0'))
            })
            .find(|ordering| *ordering != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum JsonbValue {
    Null,
    String(String),
    Number(JsonNumber),
    Bool(bool),
    Array(Vec<JsonbValue>),
    Object(BTreeMap<String, JsonbValue>),
}

struct JsonbParser<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> JsonbParser<'a> {
    fn parse(input: &'a str) -> Option<JsonbValue> {
        let mut parser = Self {
            input: input.as_bytes(),
            position: 0,
        };
        parser.skip_whitespace();
        let value = parser.parse_value()?;
        parser.skip_whitespace();
        (parser.position == parser.input.len()).then_some(value)
    }

    fn parse_value(&mut self) -> Option<JsonbValue> {
        match self.peek()? {
            b'n' => {
                self.consume_keyword(b"null")?;
                Some(JsonbValue::Null)
            }
            b't' => {
                self.consume_keyword(b"true")?;
                Some(JsonbValue::Bool(true))
            }
            b'f' => {
                self.consume_keyword(b"false")?;
                Some(JsonbValue::Bool(false))
            }
            b'"' => self.parse_string().map(JsonbValue::String),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b'-' | b'0'..=b'9' => self.parse_number().map(JsonbValue::Number),
            _ => None,
        }
    }

    fn parse_string(&mut self) -> Option<String> {
        let start = self.position;
        self.consume(b'"')?;
        let mut escaped = false;
        while let Some(byte) = self.peek() {
            self.position = self.position.checked_add(1)?;
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'"' => {
                    return serde_json::from_slice::<String>(&self.input[start..self.position])
                        .ok();
                }
                _ => {}
            }
        }
        None
    }

    fn parse_number(&mut self) -> Option<JsonNumber> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position = self.position.checked_add(1)?;
        }
        match self.peek()? {
            b'0' => {
                self.position = self.position.checked_add(1)?;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return None;
                }
            }
            b'1'..=b'9' => self.consume_digits()?,
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.position = self.position.checked_add(1)?;
            self.consume_digits()?;
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            self.position = self.position.checked_add(1)?;
            if self.peek().is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                self.position = self.position.checked_add(1)?;
            }
            self.consume_digits()?;
        }
        let text = std::str::from_utf8(&self.input[start..self.position]).ok()?;
        JsonNumber::parse(text)
    }

    fn consume_digits(&mut self) -> Option<()> {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position = self.position.checked_add(1)?;
        }
        (self.position > start).then_some(())
    }

    fn parse_array(&mut self) -> Option<JsonbValue> {
        self.consume(b'[')?;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.peek() == Some(b']') {
            self.position = self.position.checked_add(1)?;
            return Some(JsonbValue::Array(values));
        }
        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek()? {
                b',' => {
                    self.position = self.position.checked_add(1)?;
                    self.skip_whitespace();
                }
                b']' => {
                    self.position = self.position.checked_add(1)?;
                    return Some(JsonbValue::Array(values));
                }
                _ => return None,
            }
        }
    }

    fn parse_object(&mut self) -> Option<JsonbValue> {
        self.consume(b'{')?;
        self.skip_whitespace();
        let mut values = BTreeMap::new();
        if self.peek() == Some(b'}') {
            self.position = self.position.checked_add(1)?;
            return Some(JsonbValue::Object(values));
        }
        loop {
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.consume(b':')?;
            self.skip_whitespace();
            values.insert(key, self.parse_value()?);
            self.skip_whitespace();
            match self.peek()? {
                b',' => {
                    self.position = self.position.checked_add(1)?;
                    self.skip_whitespace();
                }
                b'}' => {
                    self.position = self.position.checked_add(1)?;
                    return Some(JsonbValue::Object(values));
                }
                _ => return None,
            }
        }
    }

    fn consume_keyword(&mut self, keyword: &[u8]) -> Option<()> {
        let end = self.position.checked_add(keyword.len())?;
        (self.input.get(self.position..end)? == keyword).then(|| self.position = end)
    }

    fn consume(&mut self, expected: u8) -> Option<()> {
        (self.peek()? == expected).then(|| self.position += 1)
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }
}

pub(super) fn compare_jsonb_text(left: &str, right: &str) -> Ordering {
    match (JsonbParser::parse(left), JsonbParser::parse(right)) {
        (Some(left), Some(right)) => compare_root(&left, &right),
        _ => left.cmp(right),
    }
}

fn compare_root(left: &JsonbValue, right: &JsonbValue) -> Ordering {
    match (left, right) {
        (JsonbValue::Array(left), right) if left.is_empty() && is_scalar(right) => Ordering::Less,
        (left, JsonbValue::Array(right)) if right.is_empty() && is_scalar(left) => {
            Ordering::Greater
        }
        _ => compare_value(left, right),
    }
}

fn is_scalar(value: &JsonbValue) -> bool {
    matches!(
        value,
        JsonbValue::Null | JsonbValue::Bool(_) | JsonbValue::Number(_) | JsonbValue::String(_)
    )
}

fn compare_value(left: &JsonbValue, right: &JsonbValue) -> Ordering {
    let ordering = type_rank(left).cmp(&type_rank(right));
    if ordering != Ordering::Equal {
        return ordering;
    }
    match (left, right) {
        (JsonbValue::String(left), JsonbValue::String(right)) => left.cmp(right),
        (JsonbValue::Number(left), JsonbValue::Number(right)) => left.cmp(right),
        (JsonbValue::Bool(left), JsonbValue::Bool(right)) => left.cmp(right),
        (JsonbValue::Array(left), JsonbValue::Array(right)) => {
            left.len().cmp(&right.len()).then_with(|| {
                compare_sequence(left.iter().zip(right), |(left, right)| {
                    compare_value(left, right)
                })
            })
        }
        (JsonbValue::Object(left), JsonbValue::Object(right)) => left
            .len()
            .cmp(&right.len())
            .then_with(|| compare_objects(left, right)),
        _ => Ordering::Equal,
    }
}

fn type_rank(value: &JsonbValue) -> u8 {
    match value {
        JsonbValue::Null => 0,
        JsonbValue::String(_) => 1,
        JsonbValue::Number(_) => 2,
        JsonbValue::Bool(_) => 3,
        JsonbValue::Array(_) => 4,
        JsonbValue::Object(_) => 5,
    }
}

fn compare_sequence<T>(
    values: impl IntoIterator<Item = T>,
    compare: impl Fn(T) -> Ordering,
) -> Ordering {
    values
        .into_iter()
        .map(compare)
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

fn compare_objects(
    left: &BTreeMap<String, JsonbValue>,
    right: &BTreeMap<String, JsonbValue>,
) -> Ordering {
    let mut left = left.iter().collect::<Vec<_>>();
    let mut right = right.iter().collect::<Vec<_>>();
    left.sort_unstable_by(|(left, _), (right, _)| jsonb_key_storage_order(left, right));
    right.sort_unstable_by(|(left, _), (right, _)| jsonb_key_storage_order(left, right));
    compare_sequence(
        left.into_iter().zip(right),
        |((left_key, left), (right_key, right))| {
            left_key
                .cmp(right_key)
                .then_with(|| compare_value(left, right))
        },
    )
}

fn jsonb_key_storage_order(left: &str, right: &str) -> Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

/// Return a semantic equality key for validated `jsonb` text.
#[must_use]
pub fn jsonb_equality_key(text: &str) -> Option<Vec<u8>> {
    let value = JsonbParser::parse(text)?;
    let mut output = Vec::with_capacity(text.len());
    encode_equality_value(&value, &mut output)?;
    Some(output)
}

fn encode_equality_value(value: &JsonbValue, output: &mut Vec<u8>) -> Option<()> {
    output.push(type_rank(value));
    match value {
        JsonbValue::Null => {}
        JsonbValue::Bool(value) => output.push(u8::from(*value)),
        JsonbValue::Number(value) => {
            output.push(u8::from(value.negative));
            output.extend_from_slice(&value.power.to_be_bytes());
            encode_bytes(&value.digits, output)?;
        }
        JsonbValue::String(value) => encode_bytes(value.as_bytes(), output)?,
        JsonbValue::Array(values) => {
            encode_len(values.len(), output)?;
            for value in values {
                encode_equality_value(value, output)?;
            }
        }
        JsonbValue::Object(values) => {
            encode_len(values.len(), output)?;
            for (name, value) in values {
                encode_bytes(name.as_bytes(), output)?;
                encode_equality_value(value, output)?;
            }
        }
    }
    Some(())
}

fn encode_len(length: usize, output: &mut Vec<u8>) -> Option<()> {
    output.extend_from_slice(&u64::try_from(length).ok()?.to_be_bytes());
    Some(())
}

fn encode_bytes(bytes: &[u8], output: &mut Vec<u8>) -> Option<()> {
    encode_len(bytes.len(), output)?;
    output.extend_from_slice(bytes);
    Some(())
}
