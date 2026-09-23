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
    if binding.is_some() {
        return Ok(None);
    }
    let Some(operation) = function_operation(name) else {
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
    let Some((subtype, _, multirange)) = range_identity(&first_type) else {
        return Ok(None);
    };
    Ok(Some(FunctionBinding::dispatched_with_control(
        FunctionDispatch::Range {
            operation,
            subtype,
            multirange,
        },
        control,
    )?))
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
