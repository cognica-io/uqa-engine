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

mod cache;
use cache::{retain_selection, selection_key, BuiltinSelections, SelectionTable};

/// Operator selection is a function of the operator name and its operand types without modifiers. Selections whose operands are builtin scalars or unknown, a bounded set, are made once per process; other operands are selected at every request.
static BUILTIN_SELECTIONS: BuiltinSelections = std::sync::RwLock::new(SelectionTable::new());

/// Remaining allowance that every builtin scalar selection stays below. With at least this much available, a fresh selection cannot exhaust the allowance, so a retained selection reports the same outcome; a smaller allowance selects afresh and keeps its quota errors independent of earlier selections.
const RETAINED_SELECTION_ALLOWANCE: usize = 1 << 20;

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

pub(crate) fn named_binary_operator_types(
    name: &str,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Result<[ColumnType; 3], SQLError> {
    named_binary_operator_types_with_control(name, left, right, &ProductionControl::uncontrolled())
        .map(|types| {
            types
                .into_uncontrolled()
                .expect("ordinary operator types have no reservation")
        })
}

pub(crate) fn named_binary_operator_types_with_control(
    name: &str,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<[ColumnType; 3]>, SQLError> {
    select_with_cache(name, left, right, control, &BUILTIN_SELECTIONS)
}

fn select_with_cache<const CAPACITY: usize>(
    name: &str,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
    cache: &BuiltinSelections<CAPACITY>,
) -> Result<Produced<[ColumnType; 3]>, SQLError> {
    control.check()?;
    let key = control
        .budget()
        .is_none_or(|budget| budget.available() >= RETAINED_SELECTION_ALLOWANCE)
        .then(|| selection_key(name, left, right))
        .flatten();
    let retained = key
        .as_ref()
        .and_then(|key| cache.read().ok().and_then(|selections| selections.get(key)));
    if let Some([left, right, output]) = retained {
        return result(left, right, output, control);
    }
    let selected = select_binary_operator_types(name, left, right, control)?;
    if let Some(key) = key {
        retain_selection(cache, key, &selected);
    }
    Ok(selected)
}

fn select_binary_operator_types(
    name: &str,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<[ColumnType; 3]>, SQLError> {
    let arguments = normalized_arguments(left, right, control)?;
    // One unknown operand first tries the exact known base type, before preferred-type candidate selection.
    let exact_left = arguments[0].as_ref().or(arguments[1].as_ref());
    let exact_right = arguments[1].as_ref().or(arguments[0].as_ref());
    if let (Some(lhs), Some(rhs)) = (exact_left, exact_right) {
        for &(operator, left, right, output, _, _) in SIGNATURES {
            control.check()?;
            if operator == name
                && scalar_catalog_type(left) == Some(lhs)
                && scalar_catalog_type(right) == Some(rhs)
            {
                if let Some(output) = scalar_catalog_type(output) {
                    return result(lhs, rhs, output, control);
                }
            }
        }
    }
    let candidates = candidates(name, left, right, control)?;
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
        Some("42725") => super::super::ambiguous_binary_operator(left, name, right),
        _ => undefined_binary_operator(name, left, right),
    })?;
    result(
        &selected.builtin.argument_types[0],
        &selected.builtin.argument_types[1],
        &selected.builtin.return_type,
        control,
    )
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
        if let Some([left, right]) = range_operands(lhs, rhs, [left, right]) {
            let result = catalog_type_with_control(result, control)?
                .expect("range containment has a concrete result");
            candidates.push_produced(overload(
                name,
                left.clone_with_control(control)?,
                right.clone_with_control(control)?,
                result,
                control,
            )?)?;
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
        | ColumnType::Multirange(_)
        | ColumnType::Enum(_)),
    ) = concrete
    {
        let polymorphic = match concrete {
            ColumnType::Array(_) | ColumnType::Int2Vector | ColumnType::OidVector => "anyarray",
            ColumnType::Range(_) => "anyrange",
            ColumnType::Enum(_) => "anyenum",
            _ => "anymultirange",
        };
        // `anyenum` does not accept a domain operand, while the array and range families match a domain through its base type.
        let domain_operand = polymorphic == "anyenum"
            && [left, right]
                .into_iter()
                .flatten()
                .any(|ty| matches!(ty, ColumnType::Domain { .. }));
        let consistent = !domain_operand
            && match left.zip(right) {
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

/// Instantiate asymmetric range signatures from one shared subtype; known operands must already agree with that subtype.
pub(super) fn range_operands(
    left: &str,
    right: &str,
    actual: [Option<&ColumnType>; 2],
) -> Option<[ColumnType; 2]> {
    if left == right
        || ![left, right]
            .iter()
            .any(|ty| matches!(*ty, "anyrange" | "anymultirange"))
    {
        return None;
    }
    let subtype = actual
        .into_iter()
        .flatten()
        .find_map(|ty| match base_type(ty) {
            ColumnType::Range(subtype) | ColumnType::Multirange(subtype) => Some(*subtype),
            _ => None,
        })?;
    let instantiate = |ty| match ty {
        "anyrange" => Some(ColumnType::Range(subtype)),
        "anymultirange" => Some(ColumnType::Multirange(subtype)),
        "anyelement" => Some(subtype.scalar_type()),
        _ => None,
    };
    let expected = [instantiate(left)?, instantiate(right)?];
    actual
        .into_iter()
        .zip(&expected)
        .all(|(actual, expected)| {
            actual.is_none_or(|actual| {
                crate::catalog::type_metadata::pg_type_oid(base_type(actual))
                    == crate::catalog::type_metadata::pg_type_oid(expected)
            })
        })
        .then_some(expected)
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
    if let Some(ty) = scalar_catalog_type(name) {
        return ty.clone_with_control(control).map(Some).map_err(Into::into);
    }
    match name {
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
        "anyarray" | "anyrange" | "anymultirange" | "anyenum" | "anyelement" => Ok(None),
        _ => match ColumnType::from_sql_name_with_control(name, control) {
            Ok(ty) => Ok(Some(ty)),
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
            Err(_) => Ok(None),
        },
    }
}

fn scalar_catalog_type(name: &str) -> Option<&'static ColumnType> {
    // Polymorphic declarations are instantiated from their operands, not the catalog's pseudo type.
    if matches!(
        name,
        "anyarray" | "anyrange" | "anymultirange" | "anyenum" | "anyelement"
    ) {
        return None;
    }
    crate::catalog::type_metadata::builtin_scalar_type_by_name(name)
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
                variadic: false,
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
