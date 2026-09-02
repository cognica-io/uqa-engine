//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Privilege analysis performed before MERGE begins mutation work.

use super::{BTreeSet, Engine, MergePlan, MergeWhenPlan, SQLError};

pub(super) fn ensure_merge_privileges(engine: &Engine, stmt: &MergePlan) -> Result<(), SQLError> {
    let required = stmt
        .when_clauses
        .iter()
        .filter_map(|clause| match clause {
            MergeWhenPlan::InsertNotMatched { .. } => {
                Some(crate::engine_table_security::TableAclPrivilege::Insert)
            }
            MergeWhenPlan::UpdateMatched { .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { .. } => {
                Some(crate::engine_table_security::TableAclPrivilege::Update)
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => {
                Some(crate::engine_table_security::TableAclPrivilege::Delete)
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for privilege in required {
        engine.ensure_table_privilege(&stmt.target, privilege)?;
    }

    let mut privilege_expressions = vec![&stmt.join_condition];
    privilege_expressions.extend(stmt.target_predicate.iter());
    privilege_expressions.extend(stmt.returning.iter().map(|projection| &projection.expr));
    for clause in &stmt.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
            | MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                privilege_expressions.extend(condition.iter());
                privilege_expressions
                    .extend(assignments.iter().map(|assignment| &assignment.value));
            }
            MergeWhenPlan::InsertNotMatched {
                condition, values, ..
            } => {
                privilege_expressions.extend(condition.iter());
                privilege_expressions.extend(values);
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingMatched { condition }
            | MergeWhenPlan::NothingNotMatched { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                privilege_expressions.extend(condition.iter());
            }
        }
    }
    super::super::super::ensure_target_table_select_for_expressions(
        engine,
        &stmt.target,
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        &privilege_expressions,
        false,
    )
}
