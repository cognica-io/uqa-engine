//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional USING equality retains each coercion and scalar constructor independently.

use crate::{
    ast::BinaryOp, plan::OwnedScalarExpr, production::Buffer, ColumnType, RowSchema, SQLError,
    ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub fn join_using_predicate_owned(
    using: &super::ResolvedJoinUsing,
    left: &RowSchema,
    right: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    control.check()?;
    let mut predicates = Buffer::new(control);
    for column in &using.columns {
        let lhs = column_expression(
            column.left,
            left.column_type(column.left),
            column.comparison_type.as_ref(),
            control,
        )?;
        let rhs = column_expression(
            left.len() + column.right,
            right.column_type(column.right),
            column.comparison_type.as_ref(),
            control,
        )?;
        predicates.push(
            OwnedScalarExpr::binary(BinaryOp::Equal, lhs, rhs, control)?,
            control,
        )?;
    }
    Ok(match predicates.len() {
        0 => None,
        1 => predicates.pop(),
        _ => Some(OwnedScalarExpr::boolean(
            predicates.into_iter(),
            true,
            control,
        )?),
    })
}

fn column_expression(
    position: usize,
    source: Option<&ColumnType>,
    target: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let expression = ScalarExpr::Position(position).copy_for_rewrite(control)?;
    match (source, target) {
        (Some(source), Some(target)) if source != target => Ok(OwnedScalarExpr::cast(
            expression,
            target.catalog_name_with_control(control)?,
            true,
            control,
        )?),
        _ => Ok(expression),
    }
}
