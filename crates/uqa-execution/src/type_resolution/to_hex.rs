//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible `to_hex` overload resolution.

use super::{base_type, named_argument_value, scalar_type_inner, FunctionTypeResolver};
use crate::{RowSchema, ScalarExpr};
use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::expr::{TO_HEX_INT4_FUNCTION, TO_HEX_INT8_FUNCTION};
use uqa_sql::{SQLError, SQLParam};

pub(super) fn resolve_type(
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let [argument] = args else {
        return Err(undefined_function(name, None));
    };
    let argument = named_argument_value(argument);
    let argument_type = if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
        None
    } else {
        scalar_type_inner(argument, schema, params, resolver)?
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
    name.eq_ignore_ascii_case("to_hex") || name.eq_ignore_ascii_case("pg_catalog.to_hex")
}

pub(super) fn bind_overload(
    name: String,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_some() || !is_function(&name) || args.len() != 1 {
        return name;
    }
    let Some(argument_type) =
        scalar_type_inner(named_argument_value(&args[0]), schema, params, resolver)
            .ok()
            .flatten()
    else {
        return name;
    };
    match base_type(&argument_type) {
        ColumnType::Integer => TO_HEX_INT4_FUNCTION.into(),
        ColumnType::BigInteger => TO_HEX_INT8_FUNCTION.into(),
        _ => name,
    }
}
