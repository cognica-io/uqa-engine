//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog operator selection shares `PostgreSQL` function matching/ranking and owns only the requested operator's candidate types.

use super::super::{common::base_type, BuiltinFunctionOverload};
use super::{binary_operator_name, catalog::SIGNATURES, undefined_binary_operator};
use crate::{
    ast::{BinaryOp, ColumnType},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

/// Selected declared operand types and result type for a binary SQL operator.
#[doc(hidden)]
pub fn binary_operator_types(
    op: BinaryOp,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Result<[ColumnType; 3], SQLError> {
    binary_operator_types_with_control(op, left, right, &ProductionControl::uncontrolled()).map(
        |types| {
            types
                .into_uncontrolled()
                .expect("ordinary operator types have no reservation")
        },
    )
}

/// Select through the same operator catalog while owning candidate, matching and result allocations under the caller's allowance.
#[doc(hidden)]
pub fn binary_operator_types_with_control(
    op: BinaryOp,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<[ColumnType; 3]>, SQLError> {
    named_binary_operator_types_with_control(binary_operator_name(op), left, right, control)
}

pub(super) fn named_binary_operator_types_with_control(
    name: &str,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<[ColumnType; 3]>, SQLError> {
    control.check()?;
    let candidates = candidates(name, left, right, control)?;
    let arguments = normalized_arguments(left, right, control)?;
    // One unknown operand first tries the exact known base type, before preferred-type candidate selection.
    let exact_left = arguments[0].as_ref().or(arguments[1].as_ref());
    let exact_right = arguments[1].as_ref().or(arguments[0].as_ref());
    if let (Some(lhs), Some(rhs)) = (exact_left, exact_right) {
        if let Some(candidate) = candidates.iter().find(|candidate| {
            matches!(candidate.argument_types.as_slice(), [left, right] if left == lhs && right == rhs)
        }) {
            return result(lhs, rhs, &candidate.return_type, control);
        }
    }
    let selected = super::super::overload_resolution::select_local_builtin_with_control(
        name,
        None,
        &[None, None],
        &*arguments,
        &candidates,
        control,
    )
    .map_err(|error| match error.sqlstate() {
        Some("53200" | "57014") => error,
        Some("42725") => SQLError::Routine {
            sqlstate: "42725".into(),
            message: format!(
                "operator is not unique: {} {name} {}",
                left.map_or_else(|| "unknown".into(), ColumnType::sql_name),
                right.map_or_else(|| "unknown".into(), ColumnType::sql_name)
            ),
        },
        _ => undefined_binary_operator(name, left, right),
    })?;
    let left_name = selected.builtin.argument_types[0].sql_name_with_control(control)?;
    let lhs = ColumnType::from_sql_name_with_control(&left_name, control)?;
    let right_name = selected.builtin.argument_types[1].sql_name_with_control(control)?;
    let rhs = ColumnType::from_sql_name_with_control(&right_name, control)?;
    let result = selected.builtin.return_type.clone_with_control(control)?;
    finish_types(lhs, rhs, result, control)
}

fn normalized_arguments(
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<[Option<ColumnType>; 2]>, SQLError> {
    let left = left
        .map(|ty| base_type(ty).without_type_modifiers_with_control(control))
        .transpose()?;
    let right = right
        .map(|ty| base_type(ty).without_type_modifiers_with_control(control))
        .transpose()?;
    let (left, left_memory) = left.map_or_else(
        || (None, control.empty_reservation()),
        |value| {
            let (value, memory) = value.into_parts();
            (Some(value), memory)
        },
    );
    let (right, right_memory) = right.map_or_else(
        || (None, control.empty_reservation()),
        |value| {
            let (value, memory) = value.into_parts();
            (Some(value), memory)
        },
    );
    control
        .finish([left, right], control.combine(left_memory, right_memory))
        .map_err(Into::into)
}

fn candidates(
    name: &str,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<BuiltinFunctionOverload>>, SQLError> {
    let mut candidates = ProductionVec::new(*control);
    for &(operator, lhs, rhs, result, _, _) in SIGNATURES {
        control.check()?;
        if operator != name {
            continue;
        }
        if let (Some(lhs), Some(rhs), Some(result)) = (
            catalog_type_with_control(lhs, control)?,
            catalog_type_with_control(rhs, control)?,
            catalog_type_with_control(result, control)?,
        ) {
            candidates.push_produced(overload(name, lhs, rhs, result, control)?)?;
        }
    }
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
        let consistent = match left.zip(right) {
            Some((left, right)) => {
                super::super::common::same_operator_type_with_control(left, right, control)?
            }
            None => true,
        };
        for &(operator, lhs, rhs, result, _, _) in SIGNATURES {
            control.check()?;
            if operator != name || lhs != polymorphic || rhs != polymorphic || !consistent {
                continue;
            }
            let result = if result == polymorphic {
                concrete.clone_with_control(control)?
            } else {
                catalog_type_with_control(result, control)?
                    .expect("concrete polymorphic operator result")
            };
            let lhs = left.map_or_else(
                || concrete.clone_with_control(control),
                |ty| base_type(ty).without_type_modifiers_with_control(control),
            )?;
            let rhs = right.map_or_else(
                || concrete.clone_with_control(control),
                |ty| base_type(ty).without_type_modifiers_with_control(control),
            )?;
            candidates.push_produced(overload(name, lhs, rhs, result, control)?)?;
        }
    }
    candidates.finish().map_err(Into::into)
}

pub(super) fn catalog_type(name: &str) -> Option<ColumnType> {
    catalog_type_with_control(name, &ProductionControl::uncontrolled())
        .expect("ordinary catalog type construction cannot be cancelled or limited")
        .map(|ty| {
            ty.into_uncontrolled()
                .expect("ordinary catalog type has no reservation")
        })
}

pub(super) fn catalog_type_with_control(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match name {
        "char" => control
            .finish(ColumnType::InternalChar, control.empty_reservation())
            .map(Some)
            .map_err(Into::into),
        "_text" | "_aclitem" => {
            let element = if name == "_text" {
                ColumnType::Text
            } else {
                ColumnType::AclItem
            };
            ColumnType::array_with_control(
                control.finish(element, control.empty_reservation())?,
                control,
            )
            .map(Some)
            .map_err(Into::into)
        }
        "anyarray" | "anyrange" | "anymultirange" | "anyenum" => Ok(None),
        _ => match ColumnType::from_sql_name_with_control(name, control) {
            Ok(ty) => Ok(Some(ty)),
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
            Err(_) => Ok(None),
        },
    }
}

fn overload(
    name: &str,
    left: Produced<ColumnType>,
    right: Produced<ColumnType>,
    result: Produced<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<BuiltinFunctionOverload>, SQLError> {
    let name = control.format(format_args!("pg_catalog.{name}"))?;
    let mut names = ProductionVec::new(*control);
    names.reserve(2)?;
    for _ in 0..2 {
        names.push_produced(control.finish(None, control.empty_reservation())?)?;
    }
    let names = names.finish()?;
    let mut types = ProductionVec::new(*control);
    types.reserve(2)?;
    types.push_produced(left)?;
    types.push_produced(right)?;
    let types = types.finish()?;
    let (name, name_memory) = name.into_parts();
    let (argument_names, labels_memory) = names.into_parts();
    let (argument_types, types_memory) = types.into_parts();
    let (return_type, result_memory) = result.into_parts();
    control
        .finish(
            BuiltinFunctionOverload {
                name,
                argument_names,
                argument_types,
                default_arguments: 0,
                return_type,
            },
            control.combine(
                control.combine(name_memory, labels_memory),
                control.combine(types_memory, result_memory),
            ),
        )
        .map_err(Into::into)
}

fn result(
    left: &ColumnType,
    right: &ColumnType,
    result: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<[ColumnType; 3]>, SQLError> {
    finish_types(
        left.clone_with_control(control)?,
        right.clone_with_control(control)?,
        result.clone_with_control(control)?,
        control,
    )
}

fn finish_types(
    left: Produced<ColumnType>,
    right: Produced<ColumnType>,
    result: Produced<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<[ColumnType; 3]>, SQLError> {
    let (left, left_memory) = left.into_parts();
    let (right, right_memory) = right.into_parts();
    let (result, result_memory) = result.into_parts();
    control
        .finish(
            [left, right, result],
            control.combine(control.combine(left_memory, right_memory), result_memory),
        )
        .map_err(Into::into)
}

#[cfg(test)]
mod tests;
