//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{ColumnType, ColumnTypeInputWork};
use uqa_core::{memory::ProductionControl, ValueRetentionError};

pub(in crate::type_resolution) fn same_operator_type_with_control(
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<bool, ValueRetentionError> {
    crate::catalog::work::inline(same_operator_type_with_work(left, right, control, &mut ()))
}

/// An array's operator signature identifies its scalar element, including a domain's original declaration. Differences in array dimensionality affect values only.
pub(in crate::type_resolution) async fn same_operator_type_with_work<W: ColumnTypeInputWork>(
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<bool, W::Error> {
    let left = super::base_type_with_work(left, control, work).await?;
    let right = super::base_type_with_work(right, control, work).await?;
    let (left, right) = if matches!((left, right), (ColumnType::Array(_), ColumnType::Array(_))) {
        (
            left.array_shape_with_work(control, work).await?.0,
            right.array_shape_with_work(control, work).await?.0,
        )
    } else {
        (left, right)
    };
    let left = left.without_modifiers_with_work(control, work).await?;
    let right = right.without_modifiers_with_work(control, work).await?;
    let matches = same_declaration_with_work(&left, &right, control, work).await?;
    ColumnType::retire_with_work(left, control, work).await?;
    ColumnType::retire_with_work(right, control, work).await?;
    Ok(matches)
}

pub(crate) async fn same_declaration_with_work<W: ColumnTypeInputWork>(
    mut left: &ColumnType,
    mut right: &ColumnType,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<bool, W::Error> {
    loop {
        work.position(control).await?;
        (left, right) = match (left, right) {
            (ColumnType::Array(left), ColumnType::Array(right)) => (left, right),
            (
                ColumnType::Domain {
                    oid,
                    array_oid,
                    collation_oid,
                    schema,
                    name,
                    base,
                },
                ColumnType::Domain {
                    oid: other_oid,
                    array_oid: other_array,
                    collation_oid: other_collation,
                    schema: other_schema,
                    name: other_name,
                    base: other_base,
                },
            ) => {
                if oid != other_oid
                    || array_oid != other_array
                    || collation_oid != other_collation
                    || !work.same_text(schema, other_schema, control).await?
                    || !work.same_text(name, other_name, control).await?
                {
                    return Ok(false);
                }
                (base, other_base)
            }
            (ColumnType::Named(left), ColumnType::Named(right)) => {
                return work.same_text(left, right, control).await;
            }
            _ => return Ok(left == right),
        };
    }
}
