//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric operator signatures are independent of ordinary function overloads.

use crate::ast::{
    ColumnType, FunctionBinding, FunctionResolutionError, NumericOperator, OperatorResolutionError,
};
use crate::{schema::ScalarTypeSchema, FunctionTypeResolver, SQLError, SQLParam, ScalarExpr};

use super::{resolution, UnaryOperatorCatalogEntry};

#[derive(Debug)]
pub struct NumericOperatorTypes {
    pub arguments: Vec<ColumnType>,
    pub result: ColumnType,
    pub oid: i64,
    pub function_oid: i64,
}

pub(in crate::type_resolution) fn bind_call(
    operator: NumericOperator,
    binding: &mut FunctionBinding,
    arguments: &[ScalarExpr],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    if !binding.argument_types.is_empty() || binding.resolution_error.is_some() {
        return;
    }
    let types = arguments
        .iter()
        .map(|argument| crate::common_context_expression_type(argument, schema, params, resolver))
        .collect::<Result<Vec<_>, _>>();
    let Ok(types) = types else { return };
    // A schema-free pass cannot choose a signature for a still-unresolved column, routine or subquery. Parser unknown constants can be resolved.
    if arguments.iter().zip(&types).any(|(argument, ty)| {
        ty.is_none()
            && !matches!(
                argument,
                ScalarExpr::Literal(uqa_core::Value::Str(_) | uqa_core::Value::Null)
                    | ScalarExpr::Param(_)
            )
    }) {
        return;
    }
    match numeric_operator_types(operator, &types) {
        Ok(selected) => {
            binding.argument_types = selected
                .arguments
                .iter()
                .map(ColumnType::sql_name)
                .collect();
        }
        Err(error) => {
            binding.resolution_error = Some(FunctionResolutionError::Operator(Box::new(
                OperatorResolutionError {
                    sqlstate: error.sqlstate().unwrap_or("XX000").into(),
                    message: error.to_string(),
                },
            )));
        }
    }
}

mod production;
pub use production::numeric_operator_types_with_control;

/// Select a built-in operator using its operands, without consulting the SQL routine namespace.
pub fn numeric_operator_types(
    operator: NumericOperator,
    arguments: &[Option<ColumnType>],
) -> Result<NumericOperatorTypes, SQLError> {
    numeric_operator_types_with_control(
        operator,
        arguments,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|selected| {
        selected
            .into_uncontrolled()
            .expect("ordinary operator selection has no reservation")
    })
}

pub(super) fn prefix_by_oid(oid: i64) -> Option<UnaryOperatorCatalogEntry> {
    PREFIX_SIGNATURES
        .iter()
        .find_map(|&(name, ty, candidate, function_oid)| {
            (candidate == oid).then(|| UnaryOperatorCatalogEntry {
                name,
                operand_type: resolution::catalog_type(ty).expect("static prefix operator type"),
                oid,
                function_oid,
            })
        })
}

// PostgreSQL 18.4 pg_operator identities; prefix numeric operators return their operand type.
const PREFIX_SIGNATURES: &[(&str, &str, i64, i64)] = &[
    ("+", "float4", 1919, 1913),
    ("+", "float8", 1920, 1914),
    ("+", "int2", 1917, 1911),
    ("+", "int4", 1918, 1912),
    ("+", "int8", 1916, 1910),
    ("+", "numeric", 1921, 1915),
    ("@", "float4", 590, 207),
    ("@", "float8", 595, 221),
    ("@", "int2", 682, 1253),
    ("@", "int4", 773, 1251),
    ("@", "int8", 473, 1230),
    ("@", "numeric", 1763, 1704),
    ("|/", "float8", 596, 230),
    ("||/", "float8", 597, 231),
];
