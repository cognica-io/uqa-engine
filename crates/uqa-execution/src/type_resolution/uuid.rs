//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UUID built-in overload binding while declared argument types remain available.

use uqa_core::Value;
use uqa_sql::expr::UNDEFINED_FUNCTION_MARKER;
use uqa_sql::SQLParam;

use crate::{RowSchema, ScalarExpr};

use super::common::{base_type, common_context_expression_type};
use super::FunctionTypeResolver;

pub(super) fn is_extraction_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.strip_prefix("pg_catalog.").unwrap_or(&lower),
        "uuid_extract_version" | "uuid_extract_timestamp"
    )
}

pub(super) fn bind_extraction_signature(
    name: String,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if !is_extraction_function(&name) {
        return name;
    }
    let valid = if let [argument] = args {
        !is_named_argument(argument)
            && (matches!(
                argument,
                ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
            ) || common_context_expression_type(argument, schema, params, resolver)
                .ok()
                .flatten()
                .is_some_and(|ty| matches!(base_type(&ty), uqa_sql::ast::ColumnType::Uuid)))
    } else {
        false
    };
    if valid {
        return name;
    }
    let signature = args
        .iter()
        .map(|argument| {
            common_context_expression_type(argument, schema, params, resolver)
                .ok()
                .flatten()
                .map_or_else(|| "unknown".into(), |ty| ty.sql_name())
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{UNDEFINED_FUNCTION_MARKER}{name}({signature})")
}

fn is_named_argument(expression: &ScalarExpr) -> bool {
    matches!(expression, ScalarExpr::Func { name, .. } if name == uqa_sql::expr::NAMED_ARG_FUNCTION)
}
