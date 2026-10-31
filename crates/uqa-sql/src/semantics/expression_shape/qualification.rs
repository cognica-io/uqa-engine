//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualification moves admitted scalar children and preserves their existing traversal boundaries.

use crate::{plan::OwnedScalarExpr, SQLError, ScalarExpr};
use uqa_core::memory::ProductionControl;

pub fn qualify_unqualified_columns_with_control(
    expression: &ScalarExpr,
    qualifier: &str,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    qualify(expression.copy_for_rewrite(control)?, qualifier, control)
}
fn qualify(
    expression: OwnedScalarExpr,
    qualifier: &str,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    if let ScalarExpr::Column(column) = &*expression {
        return Ok(OwnedScalarExpr::positioned_column(
            control.copy_text(column)?,
            Some(control.copy_text(qualifier)?),
            column.position(),
            control,
        )?);
    }
    // Ordered aggregate keys and window specifications belong to their separate binding scopes.
    let (arguments, filter) = match &*expression {
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => (
            Some(args.len()),
            filter.is_some().then_some(args.len() + order_by.len()),
        ),
        ScalarExpr::WindowCall { args, filter, .. } => (
            Some(args.len()),
            filter.is_some().then_some(expression.child_count() - 1),
        ),
        _ => (None, None),
    };
    expression.map_children(control, |index, child| {
        if arguments.is_some_and(|arguments| index >= arguments) && filter != Some(index) {
            Ok(child)
        } else {
            qualify(child, qualifier, control)
        }
    })
}

#[cfg(test)]
mod tests;
