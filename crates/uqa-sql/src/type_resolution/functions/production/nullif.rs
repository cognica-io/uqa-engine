//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! NULLIF selects its equality operand type while preserving dynamic UQA inputs.

use super::{copy, BinaryOp, ColumnType, Produced, ProductionControl, SQLError};
use crate::ir::ScalarCallArgument;
use crate::type_resolution::{
    common::unknown_input, operators::binary_operator_types_with_control,
};

pub(super) fn result_type(
    first: Option<&ColumnType>,
    arguments: &[ScalarCallArgument<'_>],
    effective_types: &[Option<&ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    if arguments
        .iter()
        .zip(effective_types)
        .any(|(argument, ty)| ty.is_none() && !unknown_input(argument.value))
    {
        return copy(first, control);
    }
    let effective = |position: usize| effective_types.get(position).copied().flatten();
    let selected =
        binary_operator_types_with_control(BinaryOp::Equal, effective(0), effective(1), control)?;
    if matches!(selected[0], ColumnType::Record) && first.is_some() {
        copy(first, control)
    } else {
        copy(Some(&selected[0]), control)
    }
}
