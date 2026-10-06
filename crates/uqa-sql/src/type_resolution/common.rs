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
        ColumnType::Array(element) => {
            let mut element = element.as_ref();
            while let ColumnType::Array(inner) = element {
                element = inner;
            }
            Some(element)
        }
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
            types[position] = merge_optional_types_in(
                CommonTypeContext::Values,
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
        // A runtime enum carrier knows only its type OID; declared expression types supply the enum's identity.
        Value::Null | Value::Map(_) | Value::Enum(_) => None,
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

/// `coerce_to_common_type` reads an `unknown` string constant with the selected type's input function, which reports what the type rejects, before the statement runs. A type whose input function consults the catalog is read when the expression is bound to it.
pub(super) fn read_unknown_literals<'a>(
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    target: &ColumnType,
) -> Result<(), SQLError> {
    if super::catalog_input_type(target) {
        return Ok(());
    }
    // `coerce_type` hands the literal to the target type's input function, so the diagnostic is the input function's.
    let target = base_type(target).without_type_modifiers().catalog_name();
    for expression in expressions {
        if let ScalarExpr::Literal(Value::Str(text)) = expression {
            crate::expr::cast_value_from(&Value::Str(text.clone()), &target, None)?;
        }
    }
    Ok(())
}

/// The common type of two optional column types for the construct `context`, an absent type being `unknown`.
pub(super) fn merge_optional_types_in(
    context: CommonTypeContext,
    left: Option<ColumnType>,
    right: Option<ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (None, other) | (other, None) => Ok(other),
        (Some(left), Some(right)) => common_type_in(context, &left, &right).map(Some),
    }
}

/// [`merge_value_types`] reporting a conflict as the construct `context` reports it.
pub(super) fn merge_value_types_in(
    context: CommonTypeContext,
    left: Option<Produced<ColumnType>>,
    right: Option<Produced<ColumnType>>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    match (left, right) {
        (None, other) | (other, None) => Ok(other),
        (Some(left), Some(right)) if *left == *right => Ok(Some(left)),
        (Some(left), Some(right)) => select_pair_with_control(&left, &right, control)
            .map(Some)
            .map_err(|failure| failure.in_context(context)),
    }
}

pub fn common_type(left: &ColumnType, right: &ColumnType) -> Result<ColumnType, SQLError> {
    common_type_with_control(left, right, &ProductionControl::uncontrolled()).map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary common type has no reservation")
    })
}

/// The common type of `left` and `right` for the construct `context`, which reports a conflict as `select_common_type` and `coerce_to_common_type` report it: `42804` when the types are of different categories and `42846` when the other type has no implicit cast to the selected one.
pub fn common_type_in(
    context: CommonTypeContext,
    left: &ColumnType,
    right: &ColumnType,
) -> Result<ColumnType, SQLError> {
    select_pair_with_control(left, right, &ProductionControl::uncontrolled())
        .map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary common type has no reservation")
        })
        .map_err(|failure| failure.in_context(context))
}

/// The construct selecting a common type, as `select_common_type` and `coerce_to_common_type` name it in their diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommonTypeContext {
    Union,
    Intersect,
    Except,
    Values,
    Case,
    Coalesce,
    Array,
    Greatest,
    Least,
    In,
    JoinUsing,
    Cycle,
}

impl CommonTypeContext {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Union => "UNION",
            Self::Intersect => "INTERSECT",
            Self::Except => "EXCEPT",
            Self::Values => "VALUES",
            Self::Case => "CASE",
            Self::Coalesce => "COALESCE",
            Self::Array => "ARRAY",
            Self::Greatest => "GREATEST",
            Self::Least => "LEAST",
            Self::In => "IN",
            Self::JoinUsing => "JOIN/USING",
            Self::Cycle => "CYCLE",
        }
    }

    /// The name `coerce_to_common_type` reports: `transformCaseExpr` coerces the results in a `CASE/WHEN` context.
    pub const fn coercion_label(self) -> &'static str {
        match self {
            Self::Case => "CASE/WHEN",
            other => other.label(),
        }
    }

    pub const fn set_operation(kind: crate::ast::SetOpKind) -> Self {
        match kind {
            crate::ast::SetOpKind::Union => Self::Union,
            crate::ast::SetOpKind::Intersect => Self::Intersect,
            crate::ast::SetOpKind::Except => Self::Except,
        }
    }

    /// The context of a function whose arguments share a common type.
    pub fn function(name: &str) -> Option<Self> {
        match local_routine_name(name).as_str() {
            "coalesce" => Some(Self::Coalesce),
            "greatest" => Some(Self::Greatest),
            "least" => Some(Self::Least),
            _ => None,
        }
    }
}

/// Why two types have no common type: `select_common_type` finds them in different categories, or `coerce_to_common_type` finds no implicit cast from the other type to the selected one. `Failed` carries an error the selection itself met.
#[derive(Debug)]
pub(super) enum CommonTypeFailure {
    /// The two types, in the order they met.
    Unmatched(Box<(ColumnType, ColumnType)>),
    /// The type without an implicit cast, and the selected type it would convert to.
    Inconvertible(Box<(ColumnType, ColumnType)>),
    Failed(Box<SQLError>),
}

impl CommonTypeFailure {
    fn failed(error: impl Into<SQLError>) -> Self {
        Self::Failed(Box::new(error.into()))
    }

    /// The diagnostic the construct `context` reports.
    fn in_context(self, context: CommonTypeContext) -> SQLError {
        match self {
            Self::Unmatched(types) => SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "{} types {} and {} cannot be matched",
                    context.label(),
                    types.0.sql_name(),
                    types.1.sql_name()
                ),
            },
            Self::Inconvertible(types) => SQLError::Routine {
                sqlstate: "42846".into(),
                message: format!(
                    "{} could not convert type {} to {}",
                    context.coercion_label(),
                    types.0.sql_name(),
                    types.1.sql_name()
                ),
            },
            Self::Failed(error) => *error,
        }
    }

    /// The diagnostic of a selection without a construct: a type mismatch naming the two types.
    fn without_context(self, left: &ColumnType, right: &ColumnType) -> SQLError {
        match self {
            Self::Failed(error) => *error,
            Self::Unmatched(_) | Self::Inconvertible(_) => SQLError::TypeMismatch(format!(
                "types {} and {} cannot be matched",
                left.sql_name(),
                right.sql_name()
            )),
        }
    }
}

/// `select_common_input_type_with_control` without production limits.
pub fn select_common_input_type(
    types: &[Option<&ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    select_common_input_type_with_control(types, &ProductionControl::uncontrolled()).map(
        |selected| {
            selected.map(|value| {
                value
                    .into_uncontrolled()
                    .expect("ordinary common type has no reservation")
            })
        },
    )
}

/// `select_common_type` over typed and `unknown` (`None`) inputs: only inputs of exactly one type keep that type, which is how a domain survives; otherwise domains are reduced to their base types before the pairwise rules. `unknown` inputs alone resolve to `text`, and `None` means the known types have no common type.
pub(super) fn select_common_input_type_with_control(
    types: &[Option<&ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if let Some(Some(first)) = types.first() {
        if types.iter().all(|ty| ty.is_some_and(|ty| ty == *first)) {
            return first
                .clone_with_control(control)
                .map(Some)
                .map_err(Into::into);
        }
    }
    let mut selected: Option<Produced<ColumnType>> = None;
    for ty in types.iter().flatten() {
        let ty = base_type(ty);
        selected = Some(match selected {
            None => ty.clone_with_control(control)?,
            Some(current) => match common_type_with_control(&current, ty, control) {
                Ok(common) => common,
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                Err(_) => return Ok(None),
            },
        });
    }
    match selected {
        Some(selected) => Ok(Some(selected)),
        None => control
            .finish(ColumnType::Text, control.empty_reservation())
            .map(Some)
            .map_err(Into::into),
    }
}

/// Preserve the existing common-type rules while the selected type owns its copied names and array boxes; a conflict is a type mismatch naming the two types.
pub(super) fn common_type_with_control(
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    select_pair_with_control(left, right, control)
        .map_err(|failure| failure.without_context(left, right))
}

/// `select_common_type` over two types: equal types, temporal modifiers and domains reduce first, then the numeric, OID, character, temporal and array rules, and then the category rule.
fn select_pair_with_control(
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, CommonTypeFailure> {
    control.check().map_err(CommonTypeFailure::failed)?;
    if left == right {
        return left
            .clone_with_control(control)
            .map_err(CommonTypeFailure::failed);
    }
    if left != left.without_temporal_modifiers() || right != right.without_temporal_modifiers() {
        return select_pair_with_control(
            left.without_temporal_modifiers(),
            right.without_temporal_modifiers(),
            control,
        );
    }
    if matches!(left, ColumnType::Domain { .. }) || matches!(right, ColumnType::Domain { .. }) {
        return select_pair_with_control(base_type(left), base_type(right), control);
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
            (ColumnType::Array(_), ColumnType::Array(_)) => {
                // An array type does not fix its number of dimensions: `integer[][]` is `integer[]`, so arrays meet at their element types, and each value keeps its own dimensions.
                let element = select_pair_with_control(
                    innermost_array_element(left),
                    innermost_array_element(right),
                    control,
                )?;
                return ColumnType::array_with_control(element, control)
                    .map_err(CommonTypeFailure::failed);
            }
            _ => same_category_common_type(left, right)?,
        }
    };
    control
        .finish(scalar, control.empty_reservation())
        .map_err(CommonTypeFailure::failed)
}

/// `select_common_type` for two types of one category that the rules above do not cover: the first type stays unless it is not its category's preferred type and coerces implicitly to the second, which does not coerce back to it. The other type must then coerce implicitly to the selected one, as `coerce_to_common_type` requires.
fn same_category_common_type(
    left: &ColumnType,
    right: &ColumnType,
) -> Result<ColumnType, CommonTypeFailure> {
    use super::overload_resolution::{
        routine_type_accepts_implicit_cast as implicit, routine_type_category,
        routine_type_is_preferred,
    };
    let left_name = super::canonical_column_type_name(left);
    let right_name = super::canonical_column_type_name(right);
    if routine_type_category(&left_name) != routine_type_category(&right_name) {
        return Err(CommonTypeFailure::Unmatched(Box::new((
            left.clone(),
            right.clone(),
        ))));
    }
    let switch = !routine_type_is_preferred(&left_name)
        && implicit(&left_name, &right_name)
        && !implicit(&right_name, &left_name);
    let (selected, other, selected_name, other_name) = if switch {
        (right, left, &right_name, &left_name)
    } else {
        (left, right, &left_name, &right_name)
    };
    if implicit(other_name, selected_name) {
        Ok(selected.clone())
    } else {
        Err(CommonTypeFailure::Inconvertible(Box::new((
            other.clone(),
            selected.clone(),
        ))))
    }
}

pub(super) mod case;

/// The element type below every array level of `ty`.
fn innermost_array_element(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Array(element) = ty {
        ty = element;
    }
    ty
}

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
