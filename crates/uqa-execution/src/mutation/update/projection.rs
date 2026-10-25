//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate every assignment against one qualified input before tuple locking or after a successful recheck.

use crate::mutation::assignment::MutationAssignmentTarget;
use crate::query::CteScope;
use uqa_sql::{plan::UpdatePlan, SQLError, SQLParam};

pub(super) fn project_update_row<S: Clone + 'static>(
    assignment_context: crate::mutation::assignment::MutationAssignmentContext<'_, S>,
    statement: &UpdatePlan,
    params: &[SQLParam],
    scope: &CteScope<S>,
    mut document: uqa_storage::document_store::Document,
    row: &crate::OwnedPhysicalRow,
) -> Result<uqa_storage::document_store::Document, SQLError> {
    for (position, assignment) in statement.assignments.iter().enumerate() {
        let source = crate::mutation::assignment::AssignmentSource::evaluate(
            assignment_context,
            scope,
            assignment,
            Some(row),
            params,
        )?;
        for (target_position, assignment_target) in assignment.target.targets().iter().enumerate() {
            let final_write = crate::mutation::assignment::final_column_write(
                &statement.assignments,
                position,
                target_position,
            );
            let value = crate::mutation::assignment::eval_mutation_assignment_input(
                assignment_context,
                scope,
                MutationAssignmentTarget {
                    table: &statement.table,
                    target: assignment_target,
                    current: document.get(&assignment_target.column),
                    final_column_write: final_write,
                    action: if statement.source.is_some() {
                        "UPDATE FROM"
                    } else {
                        "UPDATE"
                    },
                    new_row: false,
                },
                source.input(assignment.target.source_position(target_position)),
                Some(row),
                params,
            )?;
            if let Some(value) = value {
                document.insert(assignment_target.column.clone(), value);
            } else {
                document.remove(&assignment_target.column);
            }
        }
    }
    Ok(document)
}
