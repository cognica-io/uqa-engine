//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind range-polymorphic functions while declared range identity is available.

use super::call::{BindingCall, InferType};
use crate::ast::{
    ColumnType, FunctionBinding, FunctionDispatch, RangeFunctionOperation, RangeSubtype,
};
use crate::{SQLError, SQLParam};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

use crate::{schema::ScalarTypeSchema, ScalarExpr};

use super::{scalar_type_inner, FunctionTypeResolver};

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &[ScalarExpr],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    let control = ProductionControl::uncontrolled();
    let mut infer = |expression: &ScalarExpr| {
        scalar_type_inner(expression, schema, params, resolver)?
            .map(|ty| {
                control
                    .finish(ty, control.empty_reservation())
                    .map_err(Into::into)
            })
            .transpose()
    };
    if let Ok(Some(selected)) = select_binding(&name, binding.as_ref(), args, &mut infer, &control)
    {
        *binding = Some(
            selected
                .into_uncontrolled()
                .expect("ordinary range binding"),
        );
    }
    name
}

#[cfg(test)]
pub(super) fn bind_call_with_control(
    call: Produced<BindingCall>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<BindingCall>, SQLError> {
    let mut owner = super::call::CallOwner::new(call, control)?;
    bind_call_in_place_with_control(&mut owner.call, &mut owner.memory, infer, control)?;
    owner.finish(control)
}

/// The caller keeps the enclosing expression and its lease alive throughout mutation, including on errors and unwinding.
pub(super) fn bind_call_in_place_with_control(
    call: &mut BindingCall,
    memory: &mut Option<MemoryReservation>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    super::call::check_memory(memory.as_ref(), control)?;
    let selected = select_binding(
        &call.name,
        call.binding.as_ref(),
        &call.arguments,
        infer,
        control,
    )?;
    let Some(selected) = selected else {
        return Ok(());
    };
    let (selected, extra) = selected.into_parts();
    *memory = control.combine(memory.take(), extra);
    call.binding = Some(selected);
    control.check()?;
    Ok(())
}

fn select_binding(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<FunctionBinding>>, SQLError> {
    control.check()?;
    let previous = binding
        .filter(|binding| binding.builtin)
        .and_then(|binding| match binding.dispatch {
            Some(FunctionDispatch::Range {
                operation: RangeFunctionOperation::Contains | RangeFunctionOperation::ContainedBy,
                subtype,
                ..
            }) => Some(subtype),
            _ => None,
        });
    if binding.is_some() && previous.is_none() {
        return Ok(None);
    }
    let containment = containment_name(name, binding);
    let Some(operation) = function_operation(containment.unwrap_or(name)) else {
        return Ok(None);
    };
    let Some(first) = args.first() else {
        return Ok(None);
    };
    let first_type = match super::call::infer_with_control(first, infer, control) {
        Ok(Some(ty)) => ty,
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Ok(None) | Err(_) => return Ok(None),
    };
    let dispatch = if let Some(name) = containment.filter(|_| args.len() == 2) {
        let second_type = match super::call::infer_with_control(&args[1], infer, control) {
            Ok(Some(ty)) => ty,
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Ok(None) | Err(_) => return Ok(None),
        };
        let Some(dispatch) =
            containment_dispatch_with_control(name, &first_type, &second_type, control)?
        else {
            return Ok(None);
        };
        dispatch
    } else {
        let Some((subtype, _, multirange)) = range_identity(&first_type) else {
            return Ok(None);
        };
        FunctionDispatch::Range {
            operation,
            subtype,
            multirange,
        }
    };
    if let FunctionDispatch::Range { subtype, .. } = dispatch {
        if previous.is_some_and(|previous| previous != subtype) {
            return Ok(None);
        }
    }
    if binding.is_some_and(|binding| binding.dispatch == Some(dispatch)) {
        return Ok(None);
    }
    Ok(Some(FunctionBinding::dispatched_with_control(
        dispatch, control,
    )?))
}

/// Retained compiler bindings keep their structural operator identity even when their diagnostic label is not a SQL call name.
pub(super) fn containment_name<'a>(
    name: &'a str,
    binding: Option<&FunctionBinding>,
) -> Option<&'a str> {
    if super::containment::is_operator(name) {
        return Some(name);
    }
    match binding.and_then(|binding| binding.dispatch) {
        Some(FunctionDispatch::Range { operation, .. }) => match operation {
            RangeFunctionOperation::Contains | RangeFunctionOperation::ContainsElement => {
                Some("contains_op")
            }
            RangeFunctionOperation::ContainedBy | RangeFunctionOperation::ElementContainedBy => {
                Some("contained_by_op")
            }
            _ => None,
        },
        _ => None,
    }
}

/// Select the polymorphic signature before text carriers erase operand identity.
pub(super) fn containment_dispatch_with_control(
    name: &str,
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<FunctionDispatch>, SQLError> {
    control.check()?;
    let contains = name == "contains_op";
    let (container, element) = if contains {
        (left, right)
    } else {
        (right, left)
    };
    let Some((subtype, _, multirange)) = range_identity(container) else {
        return Ok(None);
    };
    let (operation, multirange) = if let Some((other, _, _)) = range_identity(element) {
        if subtype != other {
            return Ok(None);
        }
        let (_, _, left_multirange) = range_identity(left).expect("both operands are ranges");
        (
            if contains {
                RangeFunctionOperation::Contains
            } else {
                RangeFunctionOperation::ContainedBy
            },
            left_multirange,
        )
    } else {
        // `anyelement` retains domain identity, unlike the range operand's domain-to-base lookup. Numeric and temporal modifiers do not change its type OID.
        if *element.without_type_modifiers_with_control(control)? != subtype.scalar_type() {
            return Ok(None);
        }
        (
            if contains {
                RangeFunctionOperation::ContainsElement
            } else {
                RangeFunctionOperation::ElementContainedBy
            },
            multirange,
        )
    };
    Ok(Some(FunctionDispatch::Range {
        operation,
        subtype,
        multirange,
    }))
}

fn function_operation(name: &str) -> Option<RangeFunctionOperation> {
    let local = name.strip_prefix("pg_catalog.").unwrap_or(name);
    [
        ("lower", RangeFunctionOperation::Lower),
        ("upper", RangeFunctionOperation::Upper),
        ("isempty", RangeFunctionOperation::IsEmpty),
        ("lower_inc", RangeFunctionOperation::LowerInclusive),
        ("upper_inc", RangeFunctionOperation::UpperInclusive),
        ("lower_inf", RangeFunctionOperation::LowerInfinite),
        ("upper_inf", RangeFunctionOperation::UpperInfinite),
        ("range_merge", RangeFunctionOperation::Merge),
        ("multirange", RangeFunctionOperation::Multirange),
        ("array_overlap", RangeFunctionOperation::Overlap),
        ("contains_op", RangeFunctionOperation::Contains),
        ("contained_by_op", RangeFunctionOperation::ContainedBy),
        ("range_adjacent", RangeFunctionOperation::Adjacent),
    ]
    .into_iter()
    .find_map(|(candidate, operation)| local.eq_ignore_ascii_case(candidate).then_some(operation))
}

pub(super) fn function_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_types: &[Option<ColumnType>],
) -> Option<ColumnType> {
    if let Some(FunctionDispatch::Range {
        operation, subtype, ..
    }) = binding.and_then(|binding| binding.dispatch)
    {
        return Some(match operation {
            RangeFunctionOperation::Lower | RangeFunctionOperation::Upper => subtype.scalar_type(),
            RangeFunctionOperation::Merge => ColumnType::Range(subtype),
            RangeFunctionOperation::Multirange => ColumnType::Multirange(subtype),
            RangeFunctionOperation::IsEmpty
            | RangeFunctionOperation::LowerInclusive
            | RangeFunctionOperation::UpperInclusive
            | RangeFunctionOperation::LowerInfinite
            | RangeFunctionOperation::UpperInfinite
            | RangeFunctionOperation::Overlap
            | RangeFunctionOperation::Contains
            | RangeFunctionOperation::ContainedBy
            | RangeFunctionOperation::ContainsElement
            | RangeFunctionOperation::ElementContainedBy
            | RangeFunctionOperation::Adjacent => ColumnType::Boolean,
        });
    }
    let local = name.strip_prefix("pg_catalog.").unwrap_or(name);
    if let Some(subtype) = subtype_for_constructor(local) {
        return Some(if local.eq_ignore_ascii_case(subtype.range_name()) {
            ColumnType::Range(subtype)
        } else {
            ColumnType::Multirange(subtype)
        });
    }
    let first = argument_types.first()?.as_ref()?;
    let (subtype, _, _) = range_identity(first)?;
    Some(match function_operation(name)? {
        RangeFunctionOperation::Lower | RangeFunctionOperation::Upper => subtype.scalar_type(),
        RangeFunctionOperation::Merge => ColumnType::Range(subtype),
        RangeFunctionOperation::Multirange => ColumnType::Multirange(subtype),
        _ => ColumnType::Boolean,
    })
}

fn range_identity(ty: &ColumnType) -> Option<(RangeSubtype, &'static str, bool)> {
    match ty {
        ColumnType::Range(subtype) => Some((*subtype, subtype.range_name(), false)),
        ColumnType::Multirange(subtype) => Some((*subtype, subtype.multirange_name(), true)),
        ColumnType::Domain { base, .. } => range_identity(base),
        _ => None,
    }
}

fn subtype_for_constructor(name: &str) -> Option<RangeSubtype> {
    [
        RangeSubtype::Integer,
        RangeSubtype::BigInteger,
        RangeSubtype::Numeric,
        RangeSubtype::Date,
        RangeSubtype::Timestamp,
        RangeSubtype::TimestampTz,
    ]
    .into_iter()
    .find(|subtype| {
        name.eq_ignore_ascii_case(subtype.range_name())
            || name.eq_ignore_ascii_case(subtype.multirange_name())
    })
}
