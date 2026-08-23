//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible integer base-conversion overload resolution.

use super::common::base_type;
use super::functions::named_argument_value;
use super::{scalar_type_inner, FunctionTypeResolver};
use crate::{RowSchema, ScalarExpr};
use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::expr::{
    TO_BIN_INT4_FUNCTION, TO_BIN_INT8_FUNCTION, TO_HEX_INT4_FUNCTION, TO_HEX_INT8_FUNCTION,
    TO_OCT_INT4_FUNCTION, TO_OCT_INT8_FUNCTION,
};
use uqa_sql::{SQLError, SQLParam};

pub(super) fn resolve_type(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    let [argument] = args else {
        return Err(undefined_function(name, None));
    };
    if let Some(argument_name) = named_argument_name(argument) {
        let argument_type = argument_types.first().cloned().flatten();
        let signature = argument_type
            .as_ref()
            .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
        return Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!("function {name}({argument_name} => {signature}) does not exist"),
        });
    }
    let argument = named_argument_value(argument);
    let argument_type = if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
        None
    } else {
        argument_types.first().cloned().flatten()
    };
    match argument_type.as_ref().map(base_type) {
        Some(ColumnType::Integer | ColumnType::BigInteger) => Ok(Some(ColumnType::Text)),
        None | Some(ColumnType::SmallInteger) => Err(SQLError::Routine {
            sqlstate: "42725".into(),
            message: format!(
                "function {name}({}) is not unique",
                argument_type
                    .as_ref()
                    .map_or_else(|| "unknown".into(), ColumnType::regtype_name)
            ),
        }),
        Some(_) => Err(undefined_function(name, argument_type.as_ref())),
    }
}

fn undefined_function(name: &str, argument_type: Option<&ColumnType>) -> SQLError {
    let signature = argument_type.map_or_else(String::new, ColumnType::regtype_name);
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    }
}

pub(super) fn is_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.strip_prefix("pg_catalog.").unwrap_or(&lower),
        "to_bin" | "to_hex" | "to_oct"
    )
}

fn named_argument_name(expression: &ScalarExpr) -> Option<&str> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return None;
    }
    match args.first() {
        Some(ScalarExpr::Literal(Value::Str(name))) => Some(name),
        _ => None,
    }
}

pub(super) fn is_bound_function(name: &str) -> bool {
    matches!(
        name,
        TO_BIN_INT4_FUNCTION
            | TO_BIN_INT8_FUNCTION
            | TO_HEX_INT4_FUNCTION
            | TO_HEX_INT8_FUNCTION
            | TO_OCT_INT4_FUNCTION
            | TO_OCT_INT8_FUNCTION
    )
}

pub(super) fn bind_overload(
    name: String,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_some()
        || !is_function(&name)
        || args.len() != 1
        || named_argument_name(&args[0]).is_some()
    {
        return name;
    }
    let Some(argument_type) =
        scalar_type_inner(named_argument_value(&args[0]), schema, params, resolver)
            .ok()
            .flatten()
    else {
        return name;
    };
    let lower = name.to_ascii_lowercase();
    let function = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    match (function, base_type(&argument_type)) {
        ("to_bin", ColumnType::Integer) => TO_BIN_INT4_FUNCTION.into(),
        ("to_bin", ColumnType::BigInteger) => TO_BIN_INT8_FUNCTION.into(),
        ("to_hex", ColumnType::Integer) => TO_HEX_INT4_FUNCTION.into(),
        ("to_hex", ColumnType::BigInteger) => TO_HEX_INT8_FUNCTION.into(),
        ("to_oct", ColumnType::Integer) => TO_OCT_INT4_FUNCTION.into(),
        ("to_oct", ColumnType::BigInteger) => TO_OCT_INT8_FUNCTION.into(),
        _ => name,
    }
}
