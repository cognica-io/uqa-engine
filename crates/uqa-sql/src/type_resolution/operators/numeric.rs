//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric operator signatures are independent of ordinary function overloads.

use crate::ast::{ColumnType, FunctionBinding, NumericOperator};
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
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let mut infer = |argument: &ScalarExpr| {
        crate::common_context_expression_type(argument, schema, params, resolver)?
            .map(|ty| {
                control
                    .finish(ty, control.empty_reservation())
                    .map_err(Into::into)
            })
            .transpose()
    };
    // This legacy best-effort API leaves inference failures deferred, including errors returned by a custom resolver. The controlled entry propagates resource failures to its caller.
    let _ = bind_call_in_place_with_control(
        operator, binding, arguments, &mut None, &mut infer, &control,
    );
}

mod binding;
pub(in crate::type_resolution) use binding::bind_call_in_place_with_control;

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
