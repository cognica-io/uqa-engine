//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` containment-operator type resolution and unknown-literal coercion.

use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::{SQLError, SQLParam};

use crate::{RowSchema, ScalarExpr};

use super::common::base_type;
use super::functions::named_argument_value;
use super::{scalar_type_inner, FunctionTypeResolver};

pub(super) fn is_operator(name: &str) -> bool {
    matches!(name, "contains_op" | "contained_by_op")
}

pub(super) fn resolve_operator_type(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let left = argument_type(&args[0], argument_types[0].as_ref());
    let right = argument_type(&args[1], argument_types[1].as_ref());
    let symbol = operator_symbol(name);
    let compatible = match (&left, &right) {
        (Some(left), Some(right)) => match (base_type(left), base_type(right)) {
            (ColumnType::JsonB, ColumnType::JsonB) => true,
            (ColumnType::Array(left), ColumnType::Array(right)) => left == right,
            (
                ColumnType::Range(left) | ColumnType::Multirange(left),
                ColumnType::Range(right) | ColumnType::Multirange(right),
            ) => left == right,
            _ => false,
        },
        (Some(known), None) | (None, Some(known)) => supported_type(known),
        (None, None) => {
            return Err(SQLError::Routine {
                sqlstate: "42725".into(),
                message: format!("operator is not unique: unknown {symbol} unknown"),
            });
        }
    };
    if compatible {
        return Ok(Some(ColumnType::Boolean));
    }
    Err(SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "operator does not exist: {} {symbol} {}",
            type_name(left.as_ref()),
            type_name(right.as_ref())
        ),
    })
}

pub(super) fn bind_unknown_arguments(
    args: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    if args.len() != 2 {
        return;
    }
    let unknown = [is_unknown_literal(&args[0]), is_unknown_literal(&args[1])];
    let unknown_index = match unknown {
        [true, false] => 0,
        [false, true] => 1,
        _ => return,
    };
    let known_index = 1 - unknown_index;
    let Ok(Some(known_type)) = scalar_type_inner(
        named_argument_value(&args[known_index]),
        schema,
        params,
        resolver,
    ) else {
        return;
    };
    let target = base_type(&known_type);
    if !supported_type(target) {
        return;
    }
    let inner = std::mem::replace(&mut args[unknown_index], ScalarExpr::Literal(Value::Null));
    args[unknown_index] = ScalarExpr::Cast {
        expr: Box::new(inner),
        ty: target.sql_name(),
    };
}

fn argument_type(
    expression: &ScalarExpr,
    resolved_type: Option<&ColumnType>,
) -> Option<ColumnType> {
    if is_unknown_literal(expression) {
        None
    } else {
        resolved_type.cloned()
    }
}

fn is_unknown_literal(expression: &ScalarExpr) -> bool {
    matches!(
        named_argument_value(expression),
        ScalarExpr::Literal(Value::Str(_) | Value::Null)
    )
}

fn supported_type(ty: &ColumnType) -> bool {
    matches!(
        base_type(ty),
        ColumnType::Array(_) | ColumnType::JsonB | ColumnType::Range(_) | ColumnType::Multirange(_)
    )
}

fn type_name(ty: Option<&ColumnType>) -> String {
    ty.map_or_else(|| "unknown".into(), ColumnType::sql_name)
}

fn operator_symbol(name: &str) -> &'static str {
    if name == "contains_op" {
        "@>"
    } else {
        "<@"
    }
}
