//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` array literal parsing, shape validation, and element conversion.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
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

mod literal;

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
    parse_pg_array_literal_with_input(text, control, Ok)
}

/// Convert each completed leaf before reading later tokens, retaining both its value and its admitted owner.
pub(in crate::expr) fn parse_pg_array_literal_with_input(
    text: &str,
    control: &ProductionControl<'_>,
    mut input: impl FnMut(Produced<Value>) -> Result<Produced<Value>>,
) -> Result<Produced<ArrayValue>> {
    let mut parser = literal::ArrayLiteralParser::new(text, *control);
    let (declared_dimensions, items) = parser.parse(&mut input)?;
    match array_shape_with_control(&items, control) {
        Ok(shape) => drop(shape),
        Err(ShapeProductionError::Shape(_)) => return Err(parser.dimension_error()),
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
    .ok_or_else(|| parser.dimension_error())?;
    if let Some(declared) = declared_dimensions {
        if !declared
            .iter()
            .map(|(_, length)| *length)
            .eq(array.dimensions().iter().copied())
        {
            return Err(parser.dimension_error());
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
