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

mod inputs;
pub use inputs::select_common_input_type;
pub(crate) use inputs::select_common_input_type_in_with_control;
pub(super) use inputs::{select_common_input_type_with_control, CommonInputType};

/// Inputs whose absent type means `PostgreSQL` `unknown`, rather than an unresolved UQA runtime value.
pub(super) fn unknown_input(mut expression: &ScalarExpr) -> bool {
    while let Ok(Some(clause)) = crate::catalog::collations::expression::scalar_clause(expression) {
        expression = clause.argument;
    }
    matches!(
        expression,
        ScalarExpr::Literal(Value::Null | Value::Str(_)) | ScalarExpr::Param(_)
    )
}

/// A column or callback whose type is unavailable remains a runtime carrier; it cannot participate in overload selection as an unknown string or NULL literal.
pub(crate) fn function_arguments_have_runtime_types(
    arguments: &[ScalarExpr],
    types: &[Option<ColumnType>],
) -> Result<bool, SQLError> {
    let arguments = scalar_call_arguments(arguments)?;
    assert_eq!(arguments.len(), types.len());
    Ok(arguments
        .iter()
        .zip(types)
        .any(|(argument, ty)| runtime_input(argument.value, ty.as_ref())))
}

pub(crate) fn runtime_input(expression: &ScalarExpr, ty: Option<&ColumnType>) -> bool {
    ty.is_none() && !unknown_input(expression)
}

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

pub(crate) fn base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty.without_temporal_modifiers()
}

/// `coerce_type` reads a scalar domain through its base input, but array input keeps each element's domain checks. Input functions receive interval modifiers; other modifiers remain on the outer coercion.
pub(crate) fn literal_input_type_with_control(
    mut ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    fn input_type(
        ty: &ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, SQLError> {
        Ok(match ty {
            ColumnType::IntervalWithFields { .. } | ColumnType::Domain { .. } => {
                ty.clone_with_control(control)?
            }
            ColumnType::Array(element) => {
                ColumnType::array_with_control(input_type(element, control)?, control)?
            }
            _ => ty.without_type_modifiers_with_control(control)?,
        })
    }
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    input_type(ty, control)
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
    let control = ProductionControl::uncontrolled();
    let mut types = (0..width)
        .map(|_| CommonInputType::new())
        .collect::<Vec<_>>();
    for row in rows {
        if row.len() != width {
            return Err(SQLError::TypeMismatch(
                "VALUES lists must all be the same length".into(),
            ));
        }
        for (position, expression) in row.iter().enumerate() {
            let ty = common_context_expression_type_with_control(
                expression, &empty, params, None, &control,
            )?;
            if ty.is_some() || unknown_input(expression) {
                types[position].include_owned(CommonTypeContext::Values, ty, &control)?;
            }
        }
    }
    types
        .into_iter()
        .map(|ty| {
            ty.finish(&control).map(|ty| {
                Some(
                    ty.into_uncontrolled()
                        .expect("ordinary VALUES type has no reservation"),
                )
            })
        })
        .collect()
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
    if let Some(ty) = schema.observed_expression_type(expression) {
        return super::inference::copy_type(ty, control);
    }
    if let Some(clause) = crate::catalog::collations::expression::scalar_clause(expression)? {
        let ty = common_context_expression_type_with_control(
            clause.argument,
            schema,
            params,
            resolver,
            control,
        )?;
        crate::catalog::collations::expression::select_with_control(
            clause.name,
            clause.oid,
            ty.as_deref(),
            resolver,
            control,
        )?;
        return Ok(ty);
    }
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

mod runtime;
use runtime::RuntimeType;
pub(crate) use runtime::{parameter_type_matches, parameter_type_with_control};

pub(crate) fn value_type(value: &Value) -> Option<ColumnType> {
    value_type_with_control(value, &ProductionControl::uncontrolled())
        .expect("ordinary value type inference cannot be cancelled or limited")
        .map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary value type has no reservation")
        })
}

/// Observe an untyped host argument using its physical OID or SQL's runtime
/// carrier rules. This never reads physical payload bytes or reconstructs an
/// expression's lost declaration/modifiers. Unknown carriers return None.
pub fn runtime_value_type_with_control(
    value: &Value,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if let Value::Datum(datum) = value {
        if let Some(ty) = crate::catalog::type_metadata::builtin_scalar_type(datum.type_oid()) {
            return Ok(Some(ty.clone_with_control(control)?));
        }
        let ty = catalog
            .map(|catalog| catalog.value_type_by_oid(datum.type_oid()))
            .transpose()?
            .flatten();
        control.check()?;
        return Ok(ty
            .map(|ty| ty.retain_external_with_control(control))
            .transpose()?);
    }
    value_type_with_control(value, control)
}

pub(crate) fn value_type_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    RuntimeType::infer(value, control)?
        .map(|ty| ty.into_produced(control))
        .transpose()
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
    common_type_in_with_control(context, left, right, &ProductionControl::uncontrolled()).map(
        |value| {
            value
                .into_uncontrolled()
                .expect("ordinary common type has no reservation")
        },
    )
}

/// Select the construct's common type while admitting the resulting names and nested type payloads before copying.
pub fn common_type_in_with_control(
    context: CommonTypeContext,
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    select_pair_with_control(left, right, control).map_err(|failure| failure.in_context(context))
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
    if let (ColumnType::Array(_), ColumnType::Array(_)) = (left, right) {
        // Array dimensions belong to values; unequal array types meet at their scalar elements.
        let element = select_pair_with_control(
            innermost_array_element(left),
            innermost_array_element(right),
            control,
        )?;
        return ColumnType::array_with_control(element, control).map_err(CommonTypeFailure::failed);
    }
    let scalar = match common_scalar_type(left, right) {
        Some(scalar) => scalar,
        None => same_category_common_type(left, right)?,
    };
    control
        .finish(scalar, control.empty_reservation())
        .map_err(CommonTypeFailure::failed)
}

/// Scalar promotion shared by owned SQL types and allocation-free runtime type observation. Equal types, domains and temporal modifiers are handled by the caller before promotion.
fn common_scalar_type(left: &ColumnType, right: &ColumnType) -> Option<ColumnType> {
    if let Some(numeric) = common_numeric_type(left, right) {
        return Some(numeric);
    }
    if matches!(left, ColumnType::Oid) && is_integral_type(right)
        || matches!(right, ColumnType::Oid) && is_integral_type(left)
    {
        return Some(ColumnType::Oid);
    }
    if left.is_character_string() && right.is_character_string() {
        return Some(match left {
            ColumnType::Bpchar | ColumnType::Character(_) => ColumnType::Bpchar,
            ColumnType::Varchar(_) => ColumnType::Varchar(None),
            ColumnType::Name => ColumnType::Name,
            _ => ColumnType::Text,
        });
    }
    match (left, right) {
        (ColumnType::Date, ColumnType::Timestamp) | (ColumnType::Timestamp, ColumnType::Date) => {
            Some(ColumnType::Timestamp)
        }
        (ColumnType::Date | ColumnType::Timestamp, ColumnType::TimestampTz)
        | (ColumnType::TimestampTz, ColumnType::Date | ColumnType::Timestamp) => {
            Some(ColumnType::TimestampTz)
        }
        _ => None,
    }
}

/// `select_common_type` for two types of one category that the rules above do not cover: the first type stays unless it is not its category's preferred type and coerces implicitly to the second, which does not coerce back to it. The other type must then coerce implicitly to the selected one, as `coerce_to_common_type` requires.
fn same_category_common_type(
    left: &ColumnType,
    right: &ColumnType,
) -> Result<ColumnType, CommonTypeFailure> {
    let left_name = super::canonical_column_type_name(left);
    let right_name = super::canonical_column_type_name(right);
    match category_choice(&left_name, &right_name) {
        CategoryChoice::Left => Ok(left.clone()),
        CategoryChoice::Right => Ok(right.clone()),
        CategoryChoice::Unmatched => Err(CommonTypeFailure::Unmatched(Box::new((
            left.clone(),
            right.clone(),
        )))),
        CategoryChoice::Inconvertible => Err(CommonTypeFailure::Inconvertible(Box::new((
            right.clone(),
            left.clone(),
        )))),
    }
}

enum CategoryChoice {
    Left,
    Right,
    Unmatched,
    Inconvertible,
}

/// Both names are already canonical. Observation needs only the choice, not copied types or a diagnostic that would immediately be discarded.
fn category_choice(left: &str, right: &str) -> CategoryChoice {
    use super::overload_resolution::{
        canonical_type_category, canonical_type_is_preferred,
        routine_type_accepts_implicit_cast as implicit,
    };
    if canonical_type_category(left) != canonical_type_category(right) {
        return CategoryChoice::Unmatched;
    }
    if !canonical_type_is_preferred(left) && implicit(left, right) && !implicit(right, left) {
        return CategoryChoice::Right;
    }
    if implicit(right, left) {
        CategoryChoice::Left
    } else {
        CategoryChoice::Inconvertible
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
