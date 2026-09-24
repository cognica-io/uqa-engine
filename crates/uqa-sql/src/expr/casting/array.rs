//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` array literal parsing, shape validation, and element conversion.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    ArrayValue, Value, ValueRetentionError,
};

use crate::error::{Result, SQLError};

use super::cast_value_from_with_control;

/// Binary array coercion retains even an empty explicit dimension; element conversion constructs an ordinary dimensionless empty array.
pub(super) fn binary_compatible_elements(
    source: Option<&str>,
    target: &str,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    let Some(source) = source else {
        return Ok(true);
    };
    let source = crate::ColumnType::from_sql_name_with_control(source, control)?;
    let target = crate::ColumnType::from_sql_name_with_control(target, control)?;
    Ok(*source == *target
        || crate::type_resolution::cast_catalog_entry_with_control(&source, &target, control)?
            .is_some_and(|entry| entry.method == crate::type_resolution::CastMethod::Binary))
}

type ArrayDimensions = Produced<Vec<(i32, usize)>>;

/// Parse a `PostgreSQL` array literal (`{1,2,3}`, `{"a b",NULL}`,
/// `{{1,2},{3,4}}`) into nested lists of string/NULL values; the caller
/// casts elements.
pub fn parse_pg_array_literal(text: &str) -> Result<ArrayValue> {
    parse_pg_array_literal_with_control(text, &ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary array parse owner".into()))
}

pub fn parse_pg_array_literal_with_control(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<ArrayValue>> {
    let mut parser = PgArrayLiteralParser::new(text, *control);
    let (declared_dimensions, items) = parser.parse()?;
    match array_shape_with_control(&items, control) {
        Ok(shape) => drop(shape),
        Err(ShapeProductionError::Shape(error)) => return Err(parser.error(error.message())),
        Err(ShapeProductionError::Control(error)) => return Err(error.into()),
    }
    let array = match &declared_dimensions {
        Some(declared) => {
            let mut bounds = ProductionVec::new(*control);
            bounds.reserve(declared.len())?;
            for (lower, _) in &**declared {
                bounds.push_copy(*lower)?;
            }
            ArrayValue::with_lower_bounds_with_control(items, bounds.finish()?, control)?
        }
        None => ArrayValue::try_new_with_control(items, control)?,
    }
    .ok_or_else(|| parser.error("specified array dimensions do not match array contents"))?;
    if let Some(declared) = declared_dimensions {
        if !declared
            .iter()
            .map(|(_, length)| *length)
            .eq(array.dimensions().iter().copied())
        {
            return Err(parser.error("specified array dimensions do not match array contents"));
        }
    }
    Ok(array)
}

pub(super) fn cast_array_elements(
    items: &[Value],
    element_type: &str,
    source_element_type: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(items.len())?;
    for item in items {
        let value = match item {
            Value::List(nested) => {
                let (nested, memory) =
                    cast_array_elements(nested, element_type, source_element_type, control)?
                        .into_parts();
                control.finish(Value::List(nested), memory)?
            }
            other => {
                cast_value_from_with_control(other, element_type, source_element_type, control)?
            }
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}

pub(super) struct PgArrayLiteralParser<'a, 'control> {
    control: ProductionControl<'control>,
    source: &'a str,
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

type ParsedArrayLiteral = (Option<ArrayDimensions>, Produced<Vec<Value>>);

impl<'a, 'control> PgArrayLiteralParser<'a, 'control> {
    fn new(source: &'a str, control: ProductionControl<'control>) -> Self {
        Self {
            control,
            source,
            chars: source.chars().peekable(),
        }
    }

    fn parse(&mut self) -> Result<ParsedArrayLiteral> {
        self.skip_whitespace()?;
        let dimensions = self.parse_dimension_declaration()?;
        let items = self.parse_array()?;
        self.skip_whitespace()?;
        if self.chars.peek().is_some() {
            return Err(self.error("unexpected content after closing brace"));
        }
        Ok((dimensions, items))
    }

    fn parse_dimension_declaration(&mut self) -> Result<Option<ArrayDimensions>> {
        if self.chars.peek() != Some(&'[') {
            return Ok(None);
        }
        let mut dimensions = ProductionVec::new(self.control);
        while self.chars.next_if_eq(&'[').is_some() {
            self.skip_whitespace()?;
            let lower = self.parse_dimension_bound()?;
            self.skip_whitespace()?;
            if self.chars.next() != Some(':') {
                return Err(self.error("array dimension must contain `:`"));
            }
            self.skip_whitespace()?;
            let upper = self.parse_dimension_bound()?;
            self.skip_whitespace()?;
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
            dimensions.push_copy((lower, length))?;
            self.skip_whitespace()?;
        }
        if self.chars.next() != Some('=') {
            return Err(self.error("array dimensions must be followed by `=`"));
        }
        self.skip_whitespace()?;
        Ok(Some(dimensions.finish()?))
    }

    fn parse_dimension_bound(&mut self) -> Result<i32> {
        let mut text = ProductionString::new(self.control);
        if self
            .chars
            .peek()
            .is_some_and(|character| matches!(character, '+' | '-'))
        {
            text.push(self.chars.next().expect("peeked array bound sign"))?;
        }
        while self.chars.peek().is_some_and(char::is_ascii_digit) {
            text.push(self.chars.next().expect("peeked array bound digit"))?;
        }
        if text.is_empty() || matches!(&*text, "+" | "-") {
            return Err(self.error("array dimension bound must be an integer"));
        }
        text.parse()
            .map_err(|_| self.error("array dimension bound is out of range"))
    }

    fn parse_array(&mut self) -> Result<Produced<Vec<Value>>> {
        if self.chars.next() != Some('{') {
            return Err(self.error("array value must start with `{`"));
        }
        self.skip_whitespace()?;
        if self.chars.next_if_eq(&'}').is_some() {
            return Ok(ProductionVec::new(self.control).finish()?);
        }

        let mut items = ProductionVec::new(self.control);
        loop {
            self.skip_whitespace()?;
            items.push_produced(self.parse_element()?)?;
            self.skip_whitespace()?;
            match self.chars.next() {
                Some(',') => {
                    self.skip_whitespace()?;
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
        Ok(items.finish()?)
    }

    fn parse_element(&mut self) -> Result<Produced<Value>> {
        match self.chars.peek() {
            Some('{') => {
                let (value, memory) = self.parse_array()?.into_parts();
                Ok(self.control.finish(Value::List(value), memory)?)
            }
            Some('"') => {
                let (value, memory) = self.parse_quoted_element()?.into_parts();
                Ok(self.control.finish(Value::Str(value), memory)?)
            }
            Some(',') | Some('}') | None => Err(self.error("array contains a missing element")),
            Some(_) => self.parse_unquoted_element(),
        }
    }

    fn parse_quoted_element(&mut self) -> Result<Produced<String>> {
        let _opening_quote = self.chars.next();
        let mut value = ProductionString::new(self.control);
        loop {
            match self.chars.next() {
                Some('"') => return Ok(value.finish()?),
                Some('\\') => value.push(
                    self.chars
                        .next()
                        .ok_or_else(|| self.error("quoted element ends with an escape"))?,
                )?,
                Some(character) => value.push(character)?,
                None => return Err(self.error("array contains an unterminated quoted element")),
            }
        }
    }

    fn parse_unquoted_element(&mut self) -> Result<Produced<Value>> {
        let mut value = ProductionString::new(self.control);
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
                    value.push(escaped)?;
                    significant_len = value.len();
                    was_escaped = true;
                }
                _ => {
                    let _character = self.chars.next();
                    value.push(character)?;
                    if !character.is_whitespace() {
                        significant_len = value.len();
                    }
                }
            }
        }
        let value = value.finish()?;
        let significant = &value[..significant_len];
        if significant.is_empty() {
            return Err(self.error("array contains a missing element"));
        }
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

    fn skip_whitespace(&mut self) -> Result<()> {
        self.control.check()?;
        while self
            .chars
            .next_if(|character| character.is_whitespace())
            .is_some()
        {
            self.control.check()?;
        }
        Ok(())
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
    match array_shape_with_control(items, &ProductionControl::uncontrolled()) {
        Ok(shape) => Ok(shape.into_uncontrolled().expect("ordinary array shape")),
        Err(ShapeProductionError::Shape(error)) => Err(error),
        Err(ShapeProductionError::Control(_)) => unreachable!("ordinary shape production"),
    }
}

enum ShapeProductionError {
    Shape(ArrayShapeError),
    Control(ValueRetentionError),
}
impl From<ValueRetentionError> for ShapeProductionError {
    fn from(error: ValueRetentionError) -> Self {
        Self::Control(error)
    }
}

fn array_shape_with_control(
    items: &[Value],
    control: &ProductionControl<'_>,
) -> std::result::Result<Produced<Vec<usize>>, ShapeProductionError> {
    let mut dimensions = ProductionVec::new(*control);
    dimensions.push_copy(items.len())?;
    let mut nested_shape: Option<Produced<Vec<usize>>> = None;
    let mut has_scalar = false;
    for item in items {
        control.check()?;
        if let Value::List(nested) = item {
            let shape = array_shape_with_control(nested, control)?;
            if has_scalar {
                return Err(ShapeProductionError::Shape(ArrayShapeError::MixedNesting));
            }
            if nested_shape
                .as_ref()
                .is_some_and(|expected| **expected != *shape)
            {
                return Err(ShapeProductionError::Shape(
                    ArrayShapeError::MismatchedDimensions,
                ));
            }
            nested_shape = Some(shape);
        } else {
            if nested_shape.is_some() {
                return Err(ShapeProductionError::Shape(ArrayShapeError::MixedNesting));
            }
            has_scalar = true;
        }
    }
    if let Some(shape) = nested_shape {
        for length in &*shape {
            dimensions.push_copy(*length)?;
        }
    }
    Ok(dimensions.finish()?)
}

/// Return every dimension of a rectangular array value.
///
/// `PostgreSQL` arrays cannot mix scalar and nested elements or contain
/// sub-arrays with different extents.
pub fn array_dimensions(items: &[Value]) -> Result<Vec<usize>> {
    array_shape(items).map_err(|error| SQLError::TypeMismatch(error.message().to_string()))
}
