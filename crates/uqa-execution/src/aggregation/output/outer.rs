//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep enclosing values available after a grouped block has consumed its own rows.

use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_core::Value;
use uqa_sql::{ast::InternalRelationId, plan::QueryBlockPlan, ScalarExpr};

/// The enclosing fragment's original physical position, captured before set projections append extra local values.
#[derive(Clone)]
pub(crate) struct AggregateOuterRow {
    pub(super) row: std::sync::Arc<OwnedPhysicalRow>,
    slots: std::ops::Range<usize>,
}

impl AggregateOuterRow {
    pub(crate) fn new(
        row: &OwnedPhysicalRow,
        input_width: usize,
    ) -> Result<Self, uqa_sql::SQLError> {
        let start = input_width
            .checked_sub(row.schema.physical_width())
            .ok_or_else(|| {
                uqa_sql::SQLError::Internal("aggregate input is missing its enclosing row".into())
            })?;
        Ok(Self {
            row: std::sync::Arc::new(row.clone()),
            slots: start..input_width,
        })
    }
}

/// Group slots have already replaced selected keys, and absent grouping-set keys have already become NULL. Any remaining row input must belong to the enclosing scope, not an ungrouped local column.
pub(super) fn uses_finalized_inputs(
    expression: &ScalarExpr,
    input: &RowSchema,
    outer: Option<&AggregateOuterRow>,
    groups: InternalRelationId,
) -> bool {
    let Some(outer) = outer else {
        return false;
    };
    let mut valid = true;
    expression.visit(&mut |part| {
        let slot = match part {
            ScalarExpr::Column(name) => input.column_slot(name),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                input.qualified_slot(qualifier, column)
            }
            ScalarExpr::Position(position) => input.physical_slot(*position),
            ScalarExpr::InternalColumn(column) if column.relation() == groups => return,
            ScalarExpr::InternalColumn(column) => input.internal_slot(*column),
            ScalarExpr::Star | ScalarExpr::QualifiedStar(_) => {
                valid = false;
                return;
            }
            _ => return,
        };
        valid &= slot.is_some_and(|slot| outer.slots.contains(&slot));
    });
    valid
}

pub(super) fn group_row(
    statement: &QueryBlockPlan,
    group_values: &[Value],
    outer: Option<&OwnedPhysicalRow>,
) -> OwnedPhysicalRow {
    let group = super::group_context_row(statement, group_values);
    let Some(outer) = outer else {
        return group;
    };
    OwnedPhysicalRow::new(
        RowSchema::with_outer_schema(&group.schema, &outer.schema),
        PhysicalRow::concat_left_owned(group.row, &outer.row),
    )
}
