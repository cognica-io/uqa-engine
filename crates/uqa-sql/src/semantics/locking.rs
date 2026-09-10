//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL row-lock restrictions, propagation, and mutation lock strength.

use crate::ast::{ColumnDef, LockStrength, LockWait, LockingClause, TableKeyConstraint};
use crate::{
    plan::{
        locking::ResolvedRowLock, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan,
        SourcePlan,
    },
    SQLError,
};
use std::collections::BTreeSet;

pub mod null_rejection;

fn cte_plan_has_row_locks(body: &crate::plan::CtePlanBody) -> bool {
    match body {
        crate::plan::CtePlanBody::Query(query) => query_plan_has_row_locks(query),
        crate::plan::CtePlanBody::Command(command) => {
            command
                .ctes()
                .iter()
                .any(|cte| cte_plan_has_row_locks(&cte.body))
                || command
                    .query_inputs()
                    .into_iter()
                    .any(query_plan_has_row_locks)
                || command
                    .source_input()
                    .is_some_and(source_plan_has_row_locks)
        }
    }
}

pub fn query_plan_has_row_locks(query: &QueryPlan) -> bool {
    query
        .ctes
        .iter()
        .any(|cte| cte_plan_has_row_locks(&cte.body))
        || relational_has_row_locks(&query.root)
}

fn relational_has_row_locks(plan: &RelationalPlan) -> bool {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            !block.locking.is_empty()
                || block.from.as_ref().is_some_and(source_plan_has_row_locks)
                || block.subqueries.iter().any(query_plan_has_row_locks)
        }
        RelationalPlan::SetOp { left, right, .. } => {
            query_plan_has_row_locks(left) || query_plan_has_row_locks(right)
        }
        RelationalPlan::Values { .. } => false,
    }
}

fn source_plan_has_row_locks(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Join { left, right, .. } => {
            source_plan_has_row_locks(left) || source_plan_has_row_locks(right)
        }
        SourcePlan::Subquery { body, .. } => query_plan_has_row_locks(body),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => false,
    }
}

/// Apply a row mark selected for a stored view to the view plan before execution. Stored view plans are not present when the SQL compiler pushes row marks into derived tables, so runtime expansion must perform the same propagation to ensure an outer `NOWAIT` or `SKIP LOCKED` policy is merged before an inner row mark can block.
pub fn apply_propagated_view_lock(plan: &mut QueryPlan, target: &ResolvedRowLock) {
    apply_propagated_lock_to_relational(&mut plan.root, target.strength, target.wait);
}

fn apply_propagated_lock_to_relational(
    plan: &mut RelationalPlan,
    strength: LockStrength,
    wait: LockWait,
) {
    let RelationalPlan::QueryBlock(block) = plan else {
        return;
    };
    block.locking.push(LockingClause {
        strength,
        wait,
        relations: Vec::new(),
    });
    if let Some(source) = block.from.as_mut() {
        apply_propagated_lock_to_subqueries(source, strength, wait);
    }
}

fn apply_propagated_lock_to_subqueries(
    source: &mut SourcePlan,
    strength: LockStrength,
    wait: LockWait,
) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            apply_propagated_lock_to_subqueries(left, strength, wait);
            apply_propagated_lock_to_subqueries(right, strength, wait);
        }
        SourcePlan::Subquery { body, .. } => {
            apply_propagated_lock_to_relational(&mut body.root, strength, wait);
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

pub fn validate_locking_block_shape(
    block: &QueryBlockPlan,
    strength: LockStrength,
) -> Result<(), SQLError> {
    let label = strength.sql_name();
    if block.distinct || !block.distinct_on.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with DISTINCT clause"
        )));
    }
    if !block.group_by.is_empty() || !block.grouping_sets.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with GROUP BY clause"
        )));
    }
    if block.having.is_some() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with HAVING clause"
        )));
    }
    if matches!(block.compute, ComputePlan::Window)
        || block
            .order_by
            .iter()
            .any(|ordering| ordering.expr.contains_window())
    {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with window functions"
        )));
    }
    if matches!(block.compute, ComputePlan::Aggregate) {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with aggregate functions"
        )));
    }
    Ok(())
}

pub fn update_lock_strength(
    keys: &[TableKeyConstraint],
    definitions: &[ColumnDef],
    columns: &[String],
) -> LockStrength {
    let assigned = columns.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let touches_key = keys.iter().any(|constraint| {
        constraint.columns.iter().any(|column| {
            if assigned.contains(column.as_str()) {
                return true;
            }
            let Some(generated) = definitions
                .iter()
                .find(|definition| definition.name == *column)
                .and_then(|definition| definition.generated.as_ref())
            else {
                return false;
            };
            let mut dependencies = BTreeSet::new();
            let expression = crate::plan::ExpressionPlan::lower((*generated.expression).clone());
            !expression.scalar.collect_columns(&mut dependencies)
                || dependencies
                    .iter()
                    .any(|dependency| assigned.contains(dependency.as_str()))
        })
    });
    if touches_key {
        crate::ast::LockStrength::ForUpdate
    } else {
        crate::ast::LockStrength::ForNoKeyUpdate
    }
}
