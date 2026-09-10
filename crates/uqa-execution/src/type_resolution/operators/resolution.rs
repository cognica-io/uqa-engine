//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog operator selection shares `PostgreSQL`'s function candidate ranking.

use super::super::{common::base_type, resolve_local_builtin_overload, BuiltinFunctionOverload};
use super::{binary_operator_name, catalog::SIGNATURES, undefined_binary_operator};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use uqa_sql::ast::{BinaryOp, ColumnType};
use uqa_sql::SQLError;

/// Selected declared operand types and result type for a binary SQL operator.
#[doc(hidden)]
pub fn binary_operator_types(
    op: BinaryOp,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Result<[ColumnType; 3], SQLError> {
    let name = binary_operator_name(op);
    let mut candidates = overloads().get(name).cloned().unwrap_or_default();
    let concrete = left.or(right).map(base_type);
    if let Some(
        concrete @ (ColumnType::Array(_)
        | ColumnType::Int2Vector
        | ColumnType::OidVector
        | ColumnType::Range(_)
        | ColumnType::Multirange(_)),
    ) = concrete
    {
        let polymorphic = match concrete {
            ColumnType::Array(_) | ColumnType::Int2Vector | ColumnType::OidVector => "anyarray",
            ColumnType::Range(_) => "anyrange",
            _ => "anymultirange",
        };
        for &(operator, lhs, rhs, result) in SIGNATURES {
            let consistent = left.zip(right).is_none_or(|(left, right)| {
                base_type(left).without_type_modifiers()
                    == base_type(right).without_type_modifiers()
            });
            if operator == name && lhs == polymorphic && rhs == polymorphic && consistent {
                let result = if result == polymorphic {
                    concrete.clone()
                } else {
                    catalog_type(result).expect("concrete polymorphic operator result")
                };
                candidates.push(overload(name, concrete.clone(), concrete.clone(), result));
            }
        }
    }
    let left_base = left.map(|ty| base_type(ty).without_type_modifiers());
    let right_base = right.map(|ty| base_type(ty).without_type_modifiers());
    // A single unknown operand first tries an exact operator on the known
    // operand's base type, before preferred-type candidate selection.
    let exact_left = left_base.as_ref().or(right_base.as_ref());
    let exact_right = right_base.as_ref().or(left_base.as_ref());
    if let (Some(lhs), Some(rhs)) = (exact_left, exact_right) {
        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.argument_types == [lhs.clone(), rhs.clone()])
        {
            return Ok([lhs.clone(), rhs.clone(), candidate.return_type.clone()]);
        }
    }
    let arguments = [left_base, right_base];
    let selected =
        resolve_local_builtin_overload(name, None, &[None, None], &arguments, &candidates)
            .map_err(|error| {
                if error.sqlstate() == Some("42725") {
                    SQLError::Routine {
                        sqlstate: "42725".into(),
                        message: format!(
                            "operator is not unique: {} {name} {}",
                            left.map_or_else(|| "unknown".into(), ColumnType::sql_name),
                            right.map_or_else(|| "unknown".into(), ColumnType::sql_name)
                        ),
                    }
                } else {
                    undefined_binary_operator(op, left, right)
                }
            })?;
    let argument =
        |index: usize| ColumnType::from_sql_name(&selected.binding.argument_types[index]);
    Ok([argument(0)?, argument(1)?, selected.return_type])
}

fn catalog_type(name: &str) -> Option<ColumnType> {
    match name {
        "char" => Some(ColumnType::InternalChar),
        "_text" => Some(ColumnType::Array(Box::new(ColumnType::Text))),
        "_aclitem" => Some(ColumnType::Array(Box::new(ColumnType::AclItem))),
        "anyarray" | "anyrange" | "anymultirange" | "anyenum" => None,
        _ => ColumnType::from_sql_name(name).ok(),
    }
}

fn overload(
    name: &str,
    left: ColumnType,
    right: ColumnType,
    result: ColumnType,
) -> BuiltinFunctionOverload {
    BuiltinFunctionOverload {
        name: format!("pg_catalog.{name}"),
        argument_names: vec![None, None],
        argument_types: vec![left, right],
        default_arguments: 0,
        return_type: result,
    }
}

fn overloads() -> &'static BTreeMap<&'static str, Vec<BuiltinFunctionOverload>> {
    static OVERLOADS: OnceLock<BTreeMap<&'static str, Vec<BuiltinFunctionOverload>>> =
        OnceLock::new();
    OVERLOADS.get_or_init(|| {
        let mut operators: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for &(name, left, right, result) in SIGNATURES {
            if let (Some(left), Some(right), Some(result)) = (
                catalog_type(left),
                catalog_type(right),
                catalog_type(result),
            ) {
                operators
                    .entry(name)
                    .or_default()
                    .push(overload(name, left, right, result));
            }
        }
        operators
    })
}
