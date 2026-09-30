//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The columns a data-modifying statement supplies values for (`insertedCols` and `updatedCols`). A failing-row description shows these columns even to a role without SELECT on them, so each statement records them for the violations raised while it runs; nested statements run by triggers and routines record their own.

use std::cell::RefCell;
use std::rc::Rc;
use uqa_sql::ast::AssignmentTarget;
use uqa_sql::plan::{AssignmentPlan, ConflictActionPlan, ConflictPlan, MergePlan, MergeWhenPlan};
use uqa_sql::ScalarExpr;

thread_local! {
    static SUPPLIED_COLUMNS: RefCell<Vec<Rc<[String]>>> = const { RefCell::new(Vec::new()) };
}

/// Records one statement's supplied columns until it is dropped.
pub struct SuppliedColumnsScope;

impl SuppliedColumnsScope {
    pub fn enter(columns: Rc<[String]>) -> Self {
        SUPPLIED_COLUMNS.with(|stack| stack.borrow_mut().push(columns));
        Self
    }
}

impl Drop for SuppliedColumnsScope {
    fn drop(&mut self) {
        SUPPLIED_COLUMNS.with(|stack| {
            let removed = stack.borrow_mut().pop();
            debug_assert!(removed.is_some(), "supplied column stack underflow");
        });
    }
}

/// The innermost statement's supplied columns; a violation outside any recorded statement supplies none.
pub fn current_supplied_columns() -> Rc<[String]> {
    SUPPLIED_COLUMNS.with(|stack| {
        stack
            .borrow()
            .last()
            .cloned()
            .unwrap_or_else(|| Rc::from([]))
    })
}

/// `insertedCols` and the `ON CONFLICT DO UPDATE` targets: the columns an INSERT supplies values for. Without a column list the values fill the first `width` columns.
pub fn insert_supplied_columns(
    columns: &[AssignmentTarget<ScalarExpr>],
    width: usize,
    conflict: Option<&ConflictPlan>,
) -> Rc<[String]> {
    let mut supplied = columns
        .iter()
        .take(width)
        .map(|target| target.column.clone())
        .collect::<Vec<_>>();
    if let Some(ConflictPlan {
        action: ConflictActionPlan::Update { assignments, .. },
        ..
    }) = conflict
    {
        supplied.extend(update_targets(assignments));
    }
    supplied.into()
}

/// `updatedCols`: the SET targets of an UPDATE.
pub fn update_supplied_columns(assignments: &[AssignmentPlan]) -> Rc<[String]> {
    update_targets(assignments).collect::<Vec<_>>().into()
}

/// Every MERGE action's inserted and updated columns. An INSERT action without a column list fills the leading table columns.
pub fn merge_supplied_columns(plan: &MergePlan, table_columns: &[String]) -> Rc<[String]> {
    let mut supplied = Vec::new();
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                supplied.extend(update_targets(assignments));
            }
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                if columns.is_empty() {
                    supplied.extend(table_columns.iter().take(values.len()).cloned());
                } else {
                    supplied.extend(columns.iter().map(|target| target.column.clone()));
                }
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. }
            | MergeWhenPlan::NothingMatched { .. }
            | MergeWhenPlan::NothingNotMatched { .. }
            | MergeWhenPlan::NothingNotMatchedBySource { .. } => {}
        }
    }
    supplied.into()
}

fn update_targets(assignments: &[AssignmentPlan]) -> impl Iterator<Item = String> + '_ {
    assignments
        .iter()
        .map(|assignment| assignment.target.column.clone())
}

#[cfg(test)]
mod tests {
    use super::{current_supplied_columns, SuppliedColumnsScope};
    use std::rc::Rc;

    #[test]
    fn nested_statements_record_their_own_columns() {
        assert!(current_supplied_columns().is_empty());
        let _outer = SuppliedColumnsScope::enter(Rc::from(["a".to_string()]));
        assert_eq!(&*current_supplied_columns(), ["a"]);
        {
            let _inner = SuppliedColumnsScope::enter(Rc::from(["b".to_string(), "c".to_string()]));
            assert_eq!(&*current_supplied_columns(), ["b", "c"]);
        }
        assert_eq!(&*current_supplied_columns(), ["a"]);
    }
}
