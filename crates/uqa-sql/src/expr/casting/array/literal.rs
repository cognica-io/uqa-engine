//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array input tokens and dimensions retain `PostgreSQL` diagnostic boundaries.

use crate::{ast::MAX_ARRAY_SUBSCRIPT_DIMENSIONS, catalog::work::Work, SQLError};
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
    characters: usize,
}

impl<'a, 'control> ArrayLiteralParser<'a, 'control> {
    pub(super) fn rank(&self) -> usize {
        self.rank
    }

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
            characters: 0,
        }
    }

    pub(super) async fn parse<W: Work>(
        &mut self,
        input: &mut impl FnMut(Produced<Value>) -> Result<Produced<Value>>,
        work: &mut W,
    ) -> Result<ParsedArray> {
        self.skip_whitespace(work).await?;
        let dimensions = self.parse_dimensions(work).await?;
        if self.chars.peek() != Some(&'{') {
            return Err(self.error(if self.declared {
                "Array contents must start with \"{\"."
            } else {
                "Array value must start with \"{\" or dimension information."
            }));
        }
        let items = self.parse_array(input, work).await?;
        self.skip_whitespace(work).await?;
        if self.chars.peek().is_some() {
            return Err(self.error("Junk after closing right brace."));
        }
        // Empty nested inputs have the same dimensionless result as an empty outer array.
        if !self.has_elements {
            return Ok((None, ProductionVec::new(self.control).finish()?));
        }
        Ok((dimensions, items))
    }

    async fn parse_dimensions(&mut self, work: &mut impl Work) -> Result<Option<Dimensions>> {
        if self.chars.peek() != Some(&'[') {
            return Ok(None);
        }
        self.declared = true;
        let mut dimensions = ProductionVec::new(self.control);
        while self.chars.next_if_eq(&'[').is_some() {
            if dimensions.len() == MAX_ARRAY_SUBSCRIPT_DIMENSIONS {
                return Err(Self::too_many_dimensions());
            }
            let first = self.parse_bound(work).await?.ok_or_else(|| {
                self.error("\"[\" must introduce explicitly-specified array dimensions.")
            })?;
            let (lower, upper) = if self.chars.next_if_eq(&':').is_some() {
                let upper = self
                    .parse_bound(work)
                    .await?
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
            self.skip_whitespace(work).await?;
        }
        self.rank = dimensions.len();
        self.rank_frozen = true;
        if self.chars.next() != Some('=') {
            return Err(self.error("Missing \"=\" after array dimensions."));
        }
        self.skip_whitespace(work).await?;
        Ok(Some(dimensions.finish()?))
    }

    async fn parse_bound(&mut self, work: &mut impl Work) -> Result<Option<i32>> {
        let negative = self.chars.next_if_eq(&'-').is_some();
        if !negative {
            self.chars.next_if_eq(&'+');
        }
        let mut value = Some(0i32);
        let mut has_digit = false;
        while let Some(ch) = self.chars.next_if(char::is_ascii_digit) {
            self.character(work).await?;
            has_digit = true;
            let digit = ch as i32 - '0' as i32;
            value = value
                .and_then(|value| value.checked_mul(10))
                .and_then(|value| {
                    if negative {
                        value.checked_sub(digit)
                    } else {
                        value.checked_add(digit)
                    }
                });
        }
        if !has_digit {
            return Ok(None);
        }
        value.map(Some).ok_or_else(|| SQLError::Routine {
            sqlstate: "54000".into(),
            message: "array bound is out of integer range".into(),
        })
    }

    fn open_array(&mut self, level: usize) -> Result<ProductionVec<'control, Value>> {
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
        Ok(ProductionVec::new(self.control))
    }

    async fn parse_array<W: Work>(
        &mut self,
        input: &mut impl FnMut(Produced<Value>) -> Result<Produced<Value>>,
        work: &mut W,
    ) -> Result<Produced<Vec<Value>>> {
        let mut levels = std::array::from_fn::<_, MAX_ARRAY_SUBSCRIPT_DIMENSIONS, _>(|_| None);
        let mut level = 0;
        levels[level] = Some((self.open_array(level)?, false));
        loop {
            work.checkpoint().await?;
            let (items, expect_delimiter) = levels[level].as_mut().expect("open array");
            self.skip_whitespace(work).await?;
            match self.chars.peek().copied() {
                Some('{') => {
                    if *expect_delimiter {
                        return Err(self.error("Unexpected \"{\" character."));
                    }
                    let nested = self.open_array(level + 1)?;
                    level += 1;
                    levels[level] = Some((nested, false));
                    continue;
                }
                Some('}') => {
                    if !items.is_empty() && !*expect_delimiter {
                        return Err(self.error("Unexpected \"}\" character."));
                    }
                    let _closing = self.chars.next();
                    if self.dimensions[level].is_some_and(|length| length != items.len()) {
                        return Err(self.dimension_error());
                    }
                    self.dimensions[level] = Some(items.len());
                    let (items, _) = levels[level].take().expect("open array");
                    let items = items.finish()?;
                    if level == 0 {
                        return Ok(items);
                    }
                    level -= 1;
                    let (parent, delimiter) = levels[level].as_mut().expect("parent array");
                    let (items, memory) = items.into_parts();
                    parent.push_produced(self.control.finish(Value::List(items), memory)?)?;
                    *delimiter = true;
                    continue;
                }
                Some(',') => {
                    if !*expect_delimiter {
                        return Err(self.error("Unexpected \",\" character."));
                    }
                    let _delimiter = self.chars.next();
                    *expect_delimiter = false;
                    continue;
                }
                None => return Err(self.error("Unexpected end of input.")),
                Some(_) => {
                    let value = self.parse_element(work).await?;
                    if *expect_delimiter {
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
            *expect_delimiter = true;
        }
    }

    async fn parse_element(&mut self, work: &mut impl Work) -> Result<Produced<Value>> {
        if self.chars.peek() == Some(&'"') {
            let (value, memory) = self.parse_quoted(work).await?.into_parts();
            return Ok(self.control.finish(Value::Str(value), memory)?);
        }
        let mut value = ProductionString::new(self.control);
        let mut significant_len = 0;
        let mut was_escaped = false;
        loop {
            self.character(work).await?;
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

    async fn parse_quoted(&mut self, work: &mut impl Work) -> Result<Produced<String>> {
        let _opening_quote = self.chars.next();
        let mut value = ProductionString::new(self.control);
        loop {
            self.character(work).await?;
            match self.chars.next() {
                Some('"') => {
                    self.skip_whitespace(work).await?;
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

    async fn character(&mut self, work: &mut impl Work) -> Result<()> {
        self.characters += 1;
        if self.characters == 1024 {
            self.characters = 0;
            self.control.check()?;
            work.checkpoint().await?;
        }
        Ok(())
    }

    async fn skip_whitespace(&mut self, work: &mut impl Work) -> Result<()> {
        self.control.check()?;
        while self
            .chars
            .next_if(|character| scanner_whitespace(*character))
            .is_some()
        {
            self.character(work).await?;
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
