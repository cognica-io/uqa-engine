//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum typing and volatility in stored expressions. Parse analysis converts an `unknown` literal compared with an enum through `enum_in`, so an invalid label fails the definition; the `anyenum` support functions that read the label list are stable, and so is every I/O conversion to or from an enum.

use super::{generation_type_name, non_immutable_function, GenerationType};
use crate::ast::{ColumnType, EnumFunctionOperation, EnumTypeReference, FunctionVolatility};
use crate::schema::SchemaExpressionCatalog;
use crate::type_resolution::enums::{enum_argument_type, EnumArgumentType};
use crate::SQLError;
use uqa_core::Value;

/// Validate an unbound `anyenum` support call and select its concrete enum type. A visible user routine that matches the call exactly keeps precedence over the polymorphic built-in. The stored call stays unbound, as in a query, so evaluation binds it and converts its `unknown` arguments with the statement catalog.
pub(super) fn enum_support_call(
    engine: &dyn SchemaExpressionCatalog,
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[GenerationType],
    declared_types: &[Option<ColumnType>],
) -> Result<Option<(EnumFunctionOperation, EnumTypeReference)>, SQLError> {
    let Some((operation, reference)) = enum_call_type(name, argument_names, declared_types) else {
        return Ok(None);
    };
    if engine.lookup_visible_sql_functions(name)?.is_some() {
        let selected =
            engine.resolve_function_overload(name, None, argument_names, declared_types, false)?;
        if selected.is_some_and(|selected| {
            !selected.binding.builtin && selected.exact_matches == selected.known_arguments
        }) {
            return Ok(None);
        }
    }
    let target = GenerationType::Enum(reference.clone());
    for (position, argument) in argument_types.iter().enumerate() {
        if position < operation.enum_argument_count() {
            validate_unknown_against(engine, argument, &target)?;
        } else {
            validate_unknown_against(engine, argument, &GenerationType::BigInteger)?;
        }
    }
    if matches!(
        operation,
        EnumFunctionOperation::First
            | EnumFunctionOperation::Last
            | EnumFunctionOperation::Range
            | EnumFunctionOperation::BoundedRange
    ) {
        return Err(non_immutable_function());
    }
    Ok(Some((operation, reference)))
}

/// The operation and enum type an unbound call selects, when it is an `anyenum` support call with positional arguments of one enum type.
pub(super) fn enum_call_type(
    name: &str,
    argument_names: &[Option<String>],
    declared_types: &[Option<ColumnType>],
) -> Option<(EnumFunctionOperation, EnumTypeReference)> {
    let operation = EnumFunctionOperation::from_call(name, declared_types.len())?;
    if argument_names.iter().any(Option::is_some) {
        return None;
    }
    let borrowed = declared_types
        .iter()
        .map(Option::as_ref)
        .collect::<Vec<_>>();
    match enum_argument_type(operation, &borrowed) {
        EnumArgumentType::Enum(reference) => Some((operation, reference.clone())),
        EnumArgumentType::Unknown | EnumArgumentType::Mismatch => None,
    }
}

/// Convert an `unknown` literal to `target` as parse analysis does, reporting invalid input. Enum labels are read from the statement catalog.
pub(super) fn validate_unknown_against(
    engine: &dyn SchemaExpressionCatalog,
    value: &GenerationType,
    target: &GenerationType,
) -> Result<(), SQLError> {
    let GenerationType::UnknownLiteral(text) = value else {
        return Ok(());
    };
    let text = Value::Str(text.clone());
    match target {
        GenerationType::Enum(reference) => crate::expr::enums::fold_unknown_literal(
            crate::expr::EngineHook::enum_labels(engine),
            &text,
            &ColumnType::Enum(reference.clone()),
        )
        .map(|_| ()),
        GenerationType::Array(element) if contains_enum(element) => {
            crate::expr::enums::fold_unknown_literal(
                crate::expr::EngineHook::enum_labels(engine),
                &text,
                &enum_array_type(target),
            )
            .map(|_| ())
        }
        GenerationType::Boolean
        | GenerationType::SmallInteger
        | GenerationType::Integer
        | GenerationType::BigInteger
        | GenerationType::Real
        | GenerationType::Numeric
        | GenerationType::Uuid
        | GenerationType::Bytea
        | GenerationType::Json
        | GenerationType::JsonB
        | GenerationType::Date
        | GenerationType::Time
        | GenerationType::TimeTz
        | GenerationType::Timestamp
        | GenerationType::TimestampTz
        | GenerationType::Interval => {
            crate::expr::cast_value(&text, &generation_type_name(target)).map(|_| ())
        }
        _ => Ok(()),
    }
}

/// A cast in a stored expression must not call a mutable function; a literal of unknown type is converted during analysis and calls nothing at run time.
pub(super) fn validate_cast_volatility(
    engine: &dyn SchemaExpressionCatalog,
    source: &GenerationType,
    declared_source: Option<&ColumnType>,
    target: &ColumnType,
) -> Result<(), SQLError> {
    if matches!(source, GenerationType::UnknownLiteral(_)) {
        return validate_unknown_against(engine, source, &super::column_generation_type(target));
    }
    let Some(source) = declared_source else {
        return Ok(());
    };
    if crate::type_resolution::cast_volatility(source, target) != FunctionVolatility::Immutable {
        return Err(non_immutable_function());
    }
    Ok(())
}

/// The declared SQL type of one analyzed argument: a column's type, an enum or enum array result, or the built-in type named by its generation type. `unknown` literals and NULL have none.
pub(super) fn declared_type(
    columns: &[crate::ast::ColumnDef],
    expression: &crate::ast::Expr,
    inferred: &GenerationType,
) -> Option<ColumnType> {
    if contains_enum(inferred) && !matches!(expression, crate::ast::Expr::Column(_)) {
        return Some(enum_array_type(inferred));
    }
    super::generation_expression_column_type(columns, expression, inferred)
}

fn contains_enum(ty: &GenerationType) -> bool {
    match ty {
        GenerationType::Enum(_) => true,
        GenerationType::Array(element) => contains_enum(element),
        _ => false,
    }
}

fn enum_array_type(ty: &GenerationType) -> ColumnType {
    match ty {
        GenerationType::Enum(reference) => ColumnType::Enum(reference.clone()),
        GenerationType::Array(element) => ColumnType::Array(Box::new(enum_array_type(element))),
        _ => ColumnType::Text,
    }
}
