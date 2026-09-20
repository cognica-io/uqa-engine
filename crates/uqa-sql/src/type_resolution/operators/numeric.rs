//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric operator signatures are independent of ordinary function overloads.

use crate::ast::{
    ColumnType, FunctionBinding, FunctionResolutionError, NumericOperator, OperatorResolutionError,
};
use crate::{FunctionTypeResolver, RowSchema, SQLError, SQLParam, ScalarExpr};

use super::super::{common::base_type, resolve_local_builtin_overload, BuiltinFunctionOverload};
use super::{named_binary_operator_catalog_entry, resolution, UnaryOperatorCatalogEntry};

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
    schema: &RowSchema,
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

/// Select a built-in operator using its operands, without consulting the SQL routine namespace.
pub fn numeric_operator_types(
    operator: NumericOperator,
    arguments: &[Option<ColumnType>],
) -> Result<NumericOperatorTypes, SQLError> {
    if arguments.len() != operator.arity() {
        return Err(SQLError::Internal(format!(
            "operator {} expects {} operands, got {}",
            operator.symbol(),
            operator.arity(),
            arguments.len()
        )));
    }
    if let [left, right] = arguments {
        let [left, right, result] = resolution::named_binary_operator_types(
            operator.symbol(),
            left.as_ref(),
            right.as_ref(),
        )?;
        let entry = named_binary_operator_catalog_entry(operator.symbol(), [&left, &right])?;
        return Ok(NumericOperatorTypes {
            arguments: vec![left, right],
            result,
            oid: entry.oid,
            function_oid: entry.function_oid,
        });
    }
    let argument = arguments[0]
        .as_ref()
        .map(|ty| base_type(ty).without_type_modifiers());
    let candidates = PREFIX_SIGNATURES
        .iter()
        .filter(|&&(name, ..)| name == operator.symbol())
        .map(|&(_, ty, ..)| {
            let ty = resolution::catalog_type(ty).expect("static prefix operator type");
            BuiltinFunctionOverload {
                name: operator.symbol().into(),
                argument_names: vec![None],
                argument_types: vec![ty.clone()],
                default_arguments: 0,
                return_type: ty,
            }
        })
        .collect::<Vec<_>>();
    let selected = resolve_local_builtin_overload(
        operator.symbol(),
        None,
        &[None],
        std::slice::from_ref(&argument),
        &candidates,
    )
    .map_err(|error| {
        let ambiguous = error.sqlstate() == Some("42725");
        SQLError::Routine {
            sqlstate: if ambiguous { "42725" } else { "42883" }.into(),
            message: format!(
                "operator {}: {} {}",
                if ambiguous {
                    "is not unique"
                } else {
                    "does not exist"
                },
                operator.symbol(),
                argument
                    .as_ref()
                    .map_or_else(|| "unknown".into(), ColumnType::sql_name)
            ),
        }
    })?;
    let result = selected.return_type;
    let (_, _, oid, function_oid) = PREFIX_SIGNATURES
        .iter()
        .copied()
        .find(|&(name, ty, ..)| {
            name == operator.symbol() && resolution::catalog_type(ty).as_ref() == Some(&result)
        })
        .expect("selected prefix operator identity");
    Ok(NumericOperatorTypes {
        arguments: vec![result.clone()],
        result,
        oid,
        function_oid,
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
