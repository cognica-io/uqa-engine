//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array input tokens and dimensions retain `PostgreSQL` diagnostic boundaries.

use crate::{ast::MAX_ARRAY_SUBSCRIPT_DIMENSIONS, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    Value,
};

type Result<T> = std::result::Result<T, SQLError>;
type Dimensions = Produced<Vec<(i32, usize)>>;
type ParsedArray = (Option<Dimensions>, Produced<Vec<Value>>);

pub(super) struct ArrayLiteralParser<'a, 'control> {
    control: ProductionControl<'control>,
    source: &'a str,
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    dimensions: [Option<usize>; MAX_ARRAY_SUBSCRIPT_DIMENSIONS],
    rank: usize,
    rank_frozen: bool,
    declared: bool,
    has_elements: bool,
}

impl<'a, 'control> ArrayLiteralParser<'a, 'control> {
    pub(super) fn new(source: &'a str, control: ProductionControl<'control>) -> Self {
        Self {
            control,
            source,
            chars: source.chars().peekable(),
            dimensions: [None; MAX_ARRAY_SUBSCRIPT_DIMENSIONS],
            rank: 0,
            rank_frozen: false,
            declared: false,
            has_elements: false,
        }
    }

    pub(super) fn parse(
        &mut self,
        input: &mut impl FnMut(Produced<Value>) -> Result<Produced<Value>>,
    ) -> Result<ParsedArray> {
        self.skip_whitespace()?;
        let dimensions = self.parse_dimensions()?;
        if self.chars.peek() != Some(&'{') {
            return Err(self.error(if self.declared {
                "Array contents must start with \"{\"."
            } else {
                "Array value must start with \"{\" or dimension information."
            }));
        }
        let items = self.parse_array(0, input)?;
        self.skip_whitespace()?;
        if self.chars.peek().is_some() {
            return Err(self.error("Junk after closing right brace."));
        }
        // Empty nested inputs have the same dimensionless result as an empty outer array.
        if !self.has_elements {
            return Ok((None, ProductionVec::new(self.control).finish()?));
        }
        Ok((dimensions, items))
    }

    fn parse_dimensions(&mut self) -> Result<Option<Dimensions>> {
        if self.chars.peek() != Some(&'[') {
            return Ok(None);
        }
        self.declared = true;
        let mut dimensions = ProductionVec::new(self.control);
        while self.chars.next_if_eq(&'[').is_some() {
            if dimensions.len() == MAX_ARRAY_SUBSCRIPT_DIMENSIONS {
                return Err(Self::too_many_dimensions());
            }
            let first = self.parse_bound()?.ok_or_else(|| {
                self.error("\"[\" must introduce explicitly-specified array dimensions.")
            })?;
            let (lower, upper) = if self.chars.next_if_eq(&':').is_some() {
                let upper = self
                    .parse_bound()?
                    .ok_or_else(|| self.error("Missing array dimension value."))?;
                (first, upper)
            } else {
                (1, first)
            };
            if self.chars.next() != Some(']') {
                return Err(self.error("Missing \"]\" after array dimensions."));
            }
            if upper < lower {
                return Err(SQLError::Routine {
                    sqlstate: "2202E".into(),
                    message: "upper bound cannot be less than lower bound".into(),
                });
            }
            if upper == i32::MAX {
                return Err(SQLError::Routine {
                    sqlstate: "54000".into(),
                    message: format!("array upper bound is too large: {upper}"),
                });
            }
            let length = upper
                .checked_sub(lower)
                .and_then(|difference| difference.checked_add(1))
                .and_then(|length| usize::try_from(length).ok())
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "54000".into(),
                    message: uqa_core::ArrayAssignmentError::SizeLimit.to_string(),
                })?;
            self.dimensions[dimensions.len()] = Some(length);
            dimensions.push_copy((lower, length))?;
            self.skip_whitespace()?;
        }
        self.rank = dimensions.len();
        self.rank_frozen = true;
        if self.chars.next() != Some('=') {
            return Err(self.error("Missing \"=\" after array dimensions."));
        }
        self.skip_whitespace()?;
        Ok(Some(dimensions.finish()?))
    }

    fn parse_bound(&mut self) -> Result<Option<i32>> {
        let mut text = ProductionString::new(self.control);
        if self
            .chars
            .peek()
            .is_some_and(|character| matches!(character, '+' | '-'))
        {
            text.push(self.chars.next().expect("peeked array bound sign"))?;
        }
        let mut has_digit = false;
        while self.chars.peek().is_some_and(char::is_ascii_digit) {
            has_digit = true;
            text.push(self.chars.next().expect("peeked array bound digit"))?;
        }
        if !has_digit {
            return Ok(None);
        }
        text.parse().map(Some).map_err(|_| SQLError::Routine {
            sqlstate: "54000".into(),
            message: "array bound is out of integer range".into(),
        })
    }

    fn parse_array(
        &mut self,
        level: usize,
        input: &mut impl FnMut(Produced<Value>) -> Result<Produced<Value>>,
    ) -> Result<Produced<Vec<Value>>> {
        if level == MAX_ARRAY_SUBSCRIPT_DIMENSIONS {
            return Err(Self::too_many_dimensions());
        }
        if level >= self.rank {
            if self.rank_frozen {
                return Err(self.dimension_error());
            }
            self.rank = level + 1;
        }
        let opening = self.chars.next();
        debug_assert_eq!(opening, Some('{'));
        let mut items = ProductionVec::new(self.control);
        let mut expect_delimiter = false;
        loop {
            self.skip_whitespace()?;
            match self.chars.peek().copied() {
                Some('{') => {
                    if expect_delimiter {
                        return Err(self.error("Unexpected \"{\" character."));
                    }
                    let (nested, memory) = self.parse_array(level + 1, input)?.into_parts();
                    items.push_produced(self.control.finish(Value::List(nested), memory)?)?;
                }
                Some('}') => {
                    if !items.is_empty() && !expect_delimiter {
                        return Err(self.error("Unexpected \"}\" character."));
                    }
                    let _closing = self.chars.next();
                    if self.dimensions[level].is_some_and(|length| length != items.len()) {
                        return Err(self.dimension_error());
                    }
                    self.dimensions[level] = Some(items.len());
                    return Ok(items.finish()?);
                }
                Some(',') => {
                    if !expect_delimiter {
                        return Err(self.error("Unexpected \",\" character."));
                    }
                    let _delimiter = self.chars.next();
                    expect_delimiter = false;
                    continue;
                }
                None => return Err(self.error("Unexpected end of input.")),
                Some(_) => {
                    let value = self.parse_element()?;
                    if expect_delimiter {
                        return Err(self.error("Unexpected array element."));
                    }
                    let value = input(value)?;
                    if level + 1 != self.rank {
                        return Err(self.dimension_error());
                    }
                    self.rank_frozen = true;
                    self.has_elements = true;
                    items.push_produced(value)?;
                }
            }
            expect_delimiter = true;
        }
    }

    fn parse_element(&mut self) -> Result<Produced<Value>> {
        if self.chars.peek() == Some(&'"') {
            let (value, memory) = self.parse_quoted()?.into_parts();
            return Ok(self.control.finish(Value::Str(value), memory)?);
        }
        let mut value = ProductionString::new(self.control);
        let mut significant_len = 0;
        let mut was_escaped = false;
        loop {
            match self.chars.peek().copied() {
                Some(',' | '}') => break,
                Some('{') => return Err(self.error("Unexpected \"{\" character.")),
                Some('"') => return Err(self.error("Incorrectly quoted array element.")),
                Some('\\') => {
                    let _escape = self.chars.next();
                    let escaped = self
                        .chars
                        .next()
                        .ok_or_else(|| self.error("Unexpected end of input."))?;
                    value.push(escaped)?;
                    significant_len = value.len();
                    was_escaped = true;
                }
                Some(character) => {
                    let _character = self.chars.next();
                    value.push(character)?;
                    if !scanner_whitespace(character) {
                        significant_len = value.len();
                    }
                }
                None => return Err(self.error("Unexpected end of input.")),
            }
        }
        let value = value.finish()?;
        let significant = &value[..significant_len];
        if !was_escaped && significant.eq_ignore_ascii_case("null") {
            Ok(self
                .control
                .finish(Value::Null, self.control.empty_reservation())?)
        } else {
            let (mut value, memory) = value.into_parts();
            value.truncate(significant_len);
            Ok(self.control.finish(Value::Str(value), memory)?)
        }
    }

    fn parse_quoted(&mut self) -> Result<Produced<String>> {
        let _opening_quote = self.chars.next();
        let mut value = ProductionString::new(self.control);
        loop {
            match self.chars.next() {
                Some('"') => {
                    self.skip_whitespace()?;
                    return match self.chars.peek() {
                        Some(',' | '{' | '}') => Ok(value.finish()?),
                        None => Err(self.error("Unexpected end of input.")),
                        Some(_) => Err(self.error("Incorrectly quoted array element.")),
                    };
                }
                Some('\\') => value.push(
                    self.chars
                        .next()
                        .ok_or_else(|| self.error("Unexpected end of input."))?,
                )?,
                Some(character) => value.push(character)?,
                None => return Err(self.error("Unexpected end of input.")),
            }
        }
    }

    fn skip_whitespace(&mut self) -> Result<()> {
        self.control.check()?;
        while self
            .chars
            .next_if(|character| scanner_whitespace(*character))
            .is_some()
        {
            self.control.check()?;
        }
        Ok(())
    }

    pub(super) fn dimension_error(&self) -> SQLError {
        self.error(if self.declared {
            "Specified array dimensions do not match array contents."
        } else {
            "Multidimensional arrays must have sub-arrays with matching dimensions."
        })
    }

    fn error(&self, detail: &str) -> SQLError {
        SQLError::Diagnostic {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{}\"", self.source),
            detail: Some(detail.into()),
            hint: None,
        }
    }

    fn too_many_dimensions() -> SQLError {
        SQLError::Routine {
            sqlstate: "54000".into(),
            message: format!("number of array dimensions exceeds the maximum allowed ({MAX_ARRAY_SUBSCRIPT_DIMENSIONS})"),
        }
    }
}

fn scanner_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}
