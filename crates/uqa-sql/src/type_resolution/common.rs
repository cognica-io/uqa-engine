//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::ColumnType;
use crate::{SQLError, SQLParam};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

use crate::schema::ScalarTypeSchema;
use crate::{scalar_call_arguments, RowSchema, ScalarExpr};

use super::FunctionTypeResolver;

/// Decoded function-call argument names, effective overload types, and whether the call used explicit `VARIADIC` syntax.
#[doc(hidden)]
pub type FunctionCallArgumentSignature = (Vec<Option<String>>, Vec<Option<ColumnType>>, bool);

/// Build the PostgreSQL-compatible overload signature for one physical function call using the shared common-context typing rule.
#[doc(hidden)]
pub fn function_call_argument_signature(
    arguments: &[ScalarExpr],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<FunctionCallArgumentSignature, SQLError> {
    let call_arguments = scalar_call_arguments(arguments)?;
    let explicit_variadic = call_arguments
        .iter()
        .any(|argument| argument.explicit_variadic);
    let mut argument_names = Vec::with_capacity(call_arguments.len());
    let mut argument_types = Vec::with_capacity(call_arguments.len());
    for argument in call_arguments {
        argument_names.push(argument.name.map(str::to_string));
        let argument_type =
            common_context_expression_type(argument.value, schema, params, resolver)?;
        argument_types.push(effective_overload_argument_type_with_params(
            argument.value,
            argument_type,
            params,
        ));
    }
    Ok((argument_names, argument_types, explicit_variadic))
}

pub(super) fn local_routine_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix("pg_catalog.")
        .unwrap_or(&lower)
        .to_string()
}

pub(super) fn numeric_type() -> ColumnType {
    ColumnType::Numeric {
        precision: None,
        scale: None,
    }
}

pub(super) fn base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty.without_temporal_modifiers()
}

pub(crate) fn array_element_type(ty: &ColumnType) -> Option<&ColumnType> {
    match base_type(ty) {
        ColumnType::Array(element) => Some(element),
        ColumnType::Int2Vector => Some(&ColumnType::SmallInteger),
        ColumnType::OidVector => Some(&ColumnType::Oid),
        _ => None,
    }
}

pub fn values_column_types(
    rows: &[Vec<ScalarExpr>],
    params: &[SQLParam],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let width = rows.first().map_or(0, Vec::len);
    let empty = RowSchema::default();
    let mut types = vec![None; width];
    for row in rows {
        if row.len() != width {
            return Err(SQLError::TypeMismatch(
                "VALUES lists must all be the same length".into(),
            ));
        }
        for (position, expression) in row.iter().enumerate() {
            types[position] = merge_optional_types(
                types[position].take(),
                common_context_expression_type(expression, &empty, params, None)?,
            )?;
        }
    }
    Ok(types
        .into_iter()
        .map(|ty| ty.or(Some(ColumnType::Text)))
        .collect())
}

/// Resolve an expression participating in `PostgreSQL`'s common-type selection. Bare string and NULL literals retain the parser's `unknown` type until the surrounding VALUES, set operation, CASE, or array context selects a concrete type.
pub fn common_context_expression_type(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    common_context_expression_type_with_control(
        expression,
        schema,
        params,
        resolver,
        &ProductionControl::uncontrolled(),
    )
    .map(|ty| {
        ty.map(|ty| {
            ty.into_uncontrolled()
                .expect("ordinary common-context inference has no reservation")
        })
    })
}

pub(super) fn common_context_expression_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
        return Ok(None);
    }
    super::scalar_type_inner_with_control(expression, schema, params, resolver, control)
}

/// Preserve parser-level `unknown` identity for fixed built-in overload selection.
#[doc(hidden)]
pub fn effective_overload_argument_type(
    expression: &ScalarExpr,
    resolved: Option<ColumnType>,
) -> Option<ColumnType> {
    if effective_overload_argument_type_ref_with_params(expression, resolved.as_ref(), &[])
        .is_some()
    {
        resolved
    } else {
        None
    }
}

/// Preserve an explicitly typed scalar parameter while retaining the legacy `unknown` treatment of untyped text-valued [`SQLParam::Scalar`] parameters.
#[doc(hidden)]
pub fn effective_overload_argument_type_with_params(
    expression: &ScalarExpr,
    resolved: Option<ColumnType>,
    params: &[SQLParam],
) -> Option<ColumnType> {
    if effective_overload_argument_type_ref_with_params(expression, resolved.as_ref(), params)
        .is_some()
    {
        resolved
    } else {
        None
    }
}

/// Borrow the same effective argument type before a controlled caller decides whether a payload copy is needed.
pub(super) fn effective_overload_argument_type_ref_with_params<'a>(
    expression: &ScalarExpr,
    resolved: Option<&'a ColumnType>,
    params: &[SQLParam],
) -> Option<&'a ColumnType> {
    if let ScalarExpr::Param(index) = expression {
        if index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .is_some_and(|parameter| parameter.declared_scalar_type().is_some())
        {
            return resolved;
        }
    }
    if matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null))
        || matches!(expression, ScalarExpr::Param(_)) && matches!(resolved, Some(ColumnType::Text))
    {
        None
    } else {
        resolved
    }
}

pub(super) fn parameter_type_with_control(
    parameter: &SQLParam,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    let scalar = match parameter {
        SQLParam::Scalar(value) => return value_type_with_control(value, control),
        SQLParam::TypedScalar { ty, .. } => return Ok(Some(ty.clone_with_control(control)?)),
        SQLParam::Vector(values) => u32::try_from(values.len()).ok().map(ColumnType::Vector),
        SQLParam::Tensor(values) => values
            .first()
            .and_then(|values| u32::try_from(values.len()).ok())
            .map(ColumnType::Tensor),
    };
    scalar
        .map(|ty| {
            control
                .finish(ty, control.empty_reservation())
                .map_err(Into::into)
        })
        .transpose()
}

pub(crate) fn value_type(value: &Value) -> Option<ColumnType> {
    value_type_with_control(value, &ProductionControl::uncontrolled())
        .expect("ordinary value type inference cannot be cancelled or limited")
        .map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary value type has no reservation")
        })
}

pub(crate) fn value_type_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    let scalar = match value {
        Value::Null | Value::Map(_) => None,
        Value::Void => Some(ColumnType::Void),
        Value::Row(_) | Value::Record(_) => Some(ColumnType::Record),
        Value::Bool(_) => Some(ColumnType::Boolean),
        Value::Int(value) if i32::try_from(*value).is_ok() => Some(ColumnType::Integer),
        Value::Int(_) => Some(ColumnType::BigInteger),
        Value::Float(_) => Some(ColumnType::DoublePrecision),
        Value::Decimal(_) => Some(numeric_type()),
        Value::Str(_) => Some(ColumnType::Text),
        Value::FixedChar(value) => {
            let mut count = 0_usize;
            for _ in value.chars() {
                control.check()?;
                count += 1;
            }
            u32::try_from(count).ok().map(ColumnType::Character)
        }
        Value::Bytes(_) => Some(ColumnType::Bytea),
        Value::Temporal(value) => Some(match value {
            uqa_core::TemporalValue::Date { .. } => ColumnType::Date,
            uqa_core::TemporalValue::Time { .. } => ColumnType::Time,
            uqa_core::TemporalValue::TimeTz { .. } => ColumnType::TimeTz,
            uqa_core::TemporalValue::Timestamp { .. } => ColumnType::Timestamp,
            uqa_core::TemporalValue::TimestampTz { .. } => ColumnType::TimestampTz,
            uqa_core::TemporalValue::Interval { .. } => ColumnType::Interval,
        }),
        Value::Json(_) => Some(ColumnType::Json),
        Value::JsonB(_) => Some(ColumnType::JsonB),
        Value::LegacyVector(vector) => Some(match vector.kind() {
            uqa_core::LegacyVectorKind::SmallInteger => ColumnType::Int2Vector,
            uqa_core::LegacyVectorKind::Oid => ColumnType::OidVector,
        }),
        Value::Array(array) => {
            let mut element = None;
            if !merge_array_element_types(array.elements(), &mut element, control)? {
                return Ok(None);
            }
            return element
                .map(|element| ColumnType::array_with_control(element, control).map_err(Into::into))
                .transpose();
        }
        Value::List(values) => {
            let mut element = None;
            for value in values {
                let next = value_type_with_control(value, control)?;
                match merge_value_types(element, next, control) {
                    Ok(merged) => element = merged,
                    Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                        return Err(error)
                    }
                    Err(_) => return Ok(None),
                }
            }
            return element
                .map(|element| ColumnType::array_with_control(element, control).map_err(Into::into))
                .transpose();
        }
    };
    scalar
        .map(|ty| {
            control
                .finish(ty, control.empty_reservation())
                .map_err(Into::into)
        })
        .transpose()
}

fn merge_array_element_types(
    values: &[Value],
    element: &mut Option<Produced<ColumnType>>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for value in values {
        control.check()?;
        if let Value::List(nested) = value {
            if !merge_array_element_types(nested, element, control)? {
                return Ok(false);
            }
        } else {
            match merge_value_types(
                element.take(),
                value_type_with_control(value, control)?,
                control,
            ) {
                Ok(merged) => *element = merged,
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                Err(_) => return Ok(false),
            }
        }
    }
    Ok(true)
}

pub(super) fn merge_value_types(
    left: Option<Produced<ColumnType>>,
    right: Option<Produced<ColumnType>>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    match (left, right) {
        (None, other) | (other, None) => Ok(other),
        (Some(left), Some(right)) if *left == *right => Ok(Some(left)),
        (Some(left), Some(right)) => common_type_with_control(&left, &right, control).map(Some),
    }
}

pub(super) fn merge_optional_types(
    left: Option<ColumnType>,
    right: Option<ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (None, other) | (other, None) => Ok(other),
        (Some(left), Some(right)) => common_type(&left, &right).map(Some),
    }
}

pub fn common_type(left: &ColumnType, right: &ColumnType) -> Result<ColumnType, SQLError> {
    common_type_with_control(left, right, &ProductionControl::uncontrolled()).map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary common type has no reservation")
    })
}

/// Preserve the existing common-type rules while the selected type owns its copied names and array boxes.
pub(super) fn common_type_with_control(
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    control.check()?;
    if left == right {
        return left.clone_with_control(control).map_err(Into::into);
    }
    if left != left.without_temporal_modifiers() || right != right.without_temporal_modifiers() {
        return common_type_with_control(
            left.without_temporal_modifiers(),
            right.without_temporal_modifiers(),
            control,
        );
    }
    if matches!(left, ColumnType::Domain { .. }) || matches!(right, ColumnType::Domain { .. }) {
        return common_type_with_control(base_type(left), base_type(right), control);
    }
    let scalar = if let Some(numeric) = common_numeric_type(left, right) {
        numeric
    } else if matches!(left, ColumnType::Oid) && is_integral_type(right)
        || matches!(right, ColumnType::Oid) && is_integral_type(left)
    {
        ColumnType::Oid
    } else if left.is_character_string() && right.is_character_string() {
        match left {
            ColumnType::Bpchar | ColumnType::Character(_) => ColumnType::Bpchar,
            ColumnType::Varchar(_) => ColumnType::Varchar(None),
            ColumnType::Name => ColumnType::Name,
            _ => ColumnType::Text,
        }
    } else {
        match (left, right) {
            (ColumnType::Date, ColumnType::Timestamp)
            | (ColumnType::Timestamp, ColumnType::Date) => ColumnType::Timestamp,
            (ColumnType::Date | ColumnType::Timestamp, ColumnType::TimestampTz)
            | (ColumnType::TimestampTz, ColumnType::Date | ColumnType::Timestamp) => {
                ColumnType::TimestampTz
            }
            (ColumnType::Array(left), ColumnType::Array(right)) => {
                return ColumnType::array_with_control(
                    common_type_with_control(left, right, control)?,
                    control,
                )
                .map_err(Into::into)
            }
            _ => {
                return Err(SQLError::TypeMismatch(format!(
                    "types {} and {} cannot be matched",
                    left.sql_name(),
                    right.sql_name()
                )))
            }
        }
    };
    control
        .finish(scalar, control.empty_reservation())
        .map_err(Into::into)
}

pub(super) mod case;

fn is_integral_type(ty: &ColumnType) -> bool {
    matches!(
        base_type(ty),
        ColumnType::SmallInteger | ColumnType::Integer | ColumnType::BigInteger
    )
}

pub(super) fn common_numeric_type(left: &ColumnType, right: &ColumnType) -> Option<ColumnType> {
    let rank = numeric_rank(left)?.max(numeric_rank(right)?);
    Some(match rank {
        0 => ColumnType::SmallInteger,
        1 => ColumnType::Integer,
        2 => ColumnType::BigInteger,
        3 => numeric_type(),
        4 => ColumnType::Real,
        _ => ColumnType::DoublePrecision,
    })
}

pub(super) fn numeric_rank(ty: &ColumnType) -> Option<u8> {
    match ty {
        ColumnType::SmallInteger => Some(0),
        ColumnType::Integer => Some(1),
        ColumnType::BigInteger => Some(2),
        ColumnType::Numeric { .. } => Some(3),
        ColumnType::Real => Some(4),
        ColumnType::DoublePrecision => Some(5),
        _ => None,
    }
}

/// Array dimensions belong to values; `PostgreSQL` operator signatures identify an array by its scalar element type, including an element domain's identity.
pub(super) fn same_operator_type_with_control(
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<bool, uqa_core::ValueRetentionError> {
    fn element(mut ty: &ColumnType) -> &ColumnType {
        while let ColumnType::Array(inner) = ty {
            ty = inner;
        }
        ty
    }
    let left = base_type(left);
    let right = base_type(right);
    let (left, right) = match (left, right) {
        (ColumnType::Array(left), ColumnType::Array(right)) => (element(left), element(right)),
        _ => (left, right),
    };
    let left = left.without_type_modifiers_with_control(control)?;
    let right = right.without_type_modifiers_with_control(control)?;
    Ok(*left == *right)
}

#[cfg(test)]
mod production_tests;
