//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive scalar-expression traversal and in-place plan rewriting.

use super::{
    AssignmentPlan, CommandPlan, ConflictActionPlan, MergeWhenPlan, OrderPlan, ProjectionPlan,
    QueryPlan, RelationalPlan, ScalarExpr, ScalarFrameBound, SourcePlan,
};

pub(super) fn rewrite_query_scalars(
    query: &mut QueryPlan,
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    for cte in &mut query.ctes {
        rewrite_query_scalars(&mut cte.query, rewrite);
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                rewrite_source_scalars(source, rewrite);
            }
            rewrite_optional_scalar(&mut block.r#where, rewrite);
            for projection in &mut block.projections {
                rewrite_scalar(&mut projection.expr, rewrite);
            }
            for expression in &mut block.group_by {
                rewrite_scalar(expression, rewrite);
            }
            for set in &mut block.grouping_sets {
                for expression in set {
                    rewrite_scalar(expression, rewrite);
                }
            }
            rewrite_optional_scalar(&mut block.having, rewrite);
            rewrite_orders(&mut block.order_by, rewrite);
            rewrite_optional_scalar(&mut block.limit, rewrite);
            rewrite_optional_scalar(&mut block.offset, rewrite);
            for expression in &mut block.distinct_on {
                rewrite_scalar(expression, rewrite);
            }
            for subquery in &mut block.subqueries {
                rewrite_query_scalars(subquery, rewrite);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            rewrite_query_scalars(left, rewrite);
            rewrite_query_scalars(right, rewrite);
            rewrite_orders(order_by, rewrite);
            if let Some(limit) = limit {
                rewrite_scalar(limit, rewrite);
            }
            if let Some(offset) = offset {
                rewrite_scalar(offset, rewrite);
            }
            for subquery in subqueries {
                rewrite_query_scalars(subquery, rewrite);
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            for row in rows {
                for expression in row {
                    rewrite_scalar(expression, rewrite);
                }
            }
            for subquery in subqueries {
                rewrite_query_scalars(subquery, rewrite);
            }
        }
    }
}

pub(super) fn rewrite_source_scalars(
    source: &mut SourcePlan,
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    match source {
        SourcePlan::Table { .. } => {}
        SourcePlan::Join {
            left, right, on, ..
        } => {
            rewrite_source_scalars(left, rewrite);
            rewrite_source_scalars(right, rewrite);
            rewrite_optional_scalar(on, rewrite);
        }
        SourcePlan::Values { rows, .. } => {
            for row in rows {
                for expression in row {
                    rewrite_scalar(expression, rewrite);
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                rewrite_scalar(expression, rewrite);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                for expression in &mut function.args {
                    rewrite_scalar(expression, rewrite);
                }
            }
        }
        SourcePlan::Subquery { body, .. } => rewrite_query_scalars(body, rewrite),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "optimizer rewrite preserves exhaustive variants and fixed-point order"
)]
pub(super) fn rewrite_command_scalars(
    command: &mut CommandPlan,
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    match command {
        CommandPlan::Insert(plan) => {
            for cte in &mut plan.ctes {
                rewrite_query_scalars(&mut cte.query, rewrite);
            }
            for row in &mut plan.rows {
                for expression in row {
                    rewrite_scalar(expression, rewrite);
                }
            }
            if let Some(source) = &mut plan.source {
                rewrite_query_scalars(source, rewrite);
            }
            if let Some(conflict) = &mut plan.on_conflict {
                for expression in &mut conflict.expressions {
                    rewrite_scalar(expression, rewrite);
                }
                if let Some(predicate) = &mut conflict.predicate {
                    rewrite_scalar(predicate, rewrite);
                }
                if let ConflictActionPlan::Update {
                    assignments,
                    predicate,
                } = &mut conflict.action
                {
                    rewrite_assignments(assignments, rewrite);
                    if let Some(predicate) = predicate {
                        rewrite_scalar(predicate, rewrite);
                    }
                }
            }
            rewrite_projections(&mut plan.returning, rewrite);
            for check in &mut plan.view_checks {
                rewrite_scalar(&mut check.predicate, rewrite);
            }
            rewrite_subqueries(&mut plan.subqueries, rewrite);
        }
        CommandPlan::Update(plan) => {
            for cte in &mut plan.ctes {
                rewrite_query_scalars(&mut cte.query, rewrite);
            }
            if let Some(source) = &mut plan.source {
                rewrite_source_scalars(source, rewrite);
            }
            rewrite_assignments(&mut plan.assignments, rewrite);
            rewrite_optional_scalar(&mut plan.predicate, rewrite);
            rewrite_projections(&mut plan.returning, rewrite);
            for check in &mut plan.view_checks {
                rewrite_scalar(&mut check.predicate, rewrite);
            }
            rewrite_subqueries(&mut plan.subqueries, rewrite);
        }
        CommandPlan::Delete(plan) => {
            for cte in &mut plan.ctes {
                rewrite_query_scalars(&mut cte.query, rewrite);
            }
            if let Some(source) = &mut plan.source {
                rewrite_source_scalars(source, rewrite);
            }
            rewrite_optional_scalar(&mut plan.predicate, rewrite);
            rewrite_projections(&mut plan.returning, rewrite);
            rewrite_subqueries(&mut plan.subqueries, rewrite);
        }
        CommandPlan::Merge(plan) => {
            rewrite_source_scalars(&mut plan.source, rewrite);
            rewrite_optional_scalar(&mut plan.target_predicate, rewrite);
            rewrite_scalar(&mut plan.join_condition, rewrite);
            for clause in &mut plan.when_clauses {
                match clause {
                    MergeWhenPlan::UpdateMatched {
                        condition,
                        assignments,
                    }
                    | MergeWhenPlan::UpdateNotMatchedBySource {
                        condition,
                        assignments,
                    } => {
                        rewrite_optional_scalar(condition, rewrite);
                        rewrite_assignments(assignments, rewrite);
                    }
                    MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition }
                    | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                        rewrite_optional_scalar(condition, rewrite);
                    }
                    MergeWhenPlan::InsertNotMatched {
                        condition, values, ..
                    } => {
                        rewrite_optional_scalar(condition, rewrite);
                        for value in values {
                            rewrite_scalar(value, rewrite);
                        }
                    }
                }
            }
            rewrite_projections(&mut plan.returning, rewrite);
            for check in &mut plan.view_checks {
                rewrite_scalar(&mut check.predicate, rewrite);
            }
            rewrite_subqueries(&mut plan.subqueries, rewrite);
        }
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            rewrite_query_scalars(query, rewrite);
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            body.rewrite_scalar_expressions(rewrite);
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                rewrite_scalar(&mut expression.scalar, rewrite);
                rewrite_subqueries(&mut expression.subqueries, rewrite);
            }
        }
        CommandPlan::CreateTable(_)
        | CommandPlan::CreateTableIfNotExists(_)
        | CommandPlan::CreateIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::AlterTable(_)
        | CommandPlan::AlterForeignTable(_)
        | CommandPlan::AlterView(_)
        | CommandPlan::RefreshMaterializedView { .. }
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::Notify { .. }
        | CommandPlan::Listen { .. }
        | CommandPlan::Unlisten { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ResetVariable { .. }
        | CommandPlan::ResetAllVariables
        | CommandPlan::SetConstraints { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Load { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::Vacuum(_)
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::FetchCursor(_)
        | CommandPlan::CloseCursor { .. }
        | CommandPlan::CreateSequence(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateForeignTableIfNotExists(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::AlterRoutine(_)
        | CommandPlan::AlterRoutineOwner(_)
        | CommandPlan::RenameRoutine(_)
        | CommandPlan::GrantRoutine(_)
        | CommandPlan::GrantTable(_)
        | CommandPlan::GrantSequence(_)
        | CommandPlan::GrantDatabase(_)
        | CommandPlan::GrantSchema(_)
        | CommandPlan::GrantRole(_)
        | CommandPlan::CreateRole(_)
        | CommandPlan::AlterRole(_)
        | CommandPlan::DropRole(_)
        | CommandPlan::CreateTrigger(_)
        | CommandPlan::DropTrigger(_)
        | CommandPlan::CreateRule(_)
        | CommandPlan::DropRule(_)
        | CommandPlan::DoBlock { .. } => {}
    }
}

pub(super) fn rewrite_assignments(
    assignments: &mut [AssignmentPlan],
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    for assignment in assignments {
        rewrite_scalar(&mut assignment.value, rewrite);
    }
}

pub(super) fn rewrite_projections(
    projections: &mut [ProjectionPlan],
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    for projection in projections {
        rewrite_scalar(&mut projection.expr, rewrite);
    }
}

pub(super) fn rewrite_orders(orders: &mut [OrderPlan], rewrite: &mut dyn FnMut(&mut ScalarExpr)) {
    for order in orders {
        rewrite_scalar(&mut order.expr, rewrite);
    }
}

pub(super) fn rewrite_subqueries(
    subqueries: &mut [QueryPlan],
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    for subquery in subqueries {
        rewrite_query_scalars(subquery, rewrite);
    }
}

pub(super) fn rewrite_optional_scalar(
    expression: &mut Option<ScalarExpr>,
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    if let Some(expression) = expression {
        rewrite_scalar(expression, rewrite);
    }
}

pub(super) fn rewrite_scalar(
    expression: &mut ScalarExpr,
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    match expression {
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                rewrite_scalar(argument, rewrite);
            }
            for order in order_by {
                rewrite_scalar(&mut order.expr, rewrite);
            }
            if let Some(filter) = filter {
                rewrite_scalar(filter, rewrite);
            }
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                rewrite_scalar(item, rewrite);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            rewrite_scalar(lhs, rewrite);
            rewrite_scalar(rhs, rewrite);
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => rewrite_scalar(inner, rewrite),
        ScalarExpr::Between { expr, low, high } => {
            rewrite_scalar(expr, rewrite);
            rewrite_scalar(low, rewrite);
            rewrite_scalar(high, rewrite);
        }
        ScalarExpr::InList { expr, list, .. } => {
            rewrite_scalar(expr, rewrite);
            for item in list {
                rewrite_scalar(item, rewrite);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                rewrite_scalar(argument, rewrite);
            }
            for expression in &mut spec.partition_by {
                rewrite_scalar(expression, rewrite);
            }
            for order in &mut spec.order_by {
                rewrite_scalar(&mut order.expr, rewrite);
            }
            if let Some(frame) = &mut spec.frame {
                rewrite_frame_bound(&mut frame.start, rewrite);
                rewrite_frame_bound(&mut frame.end, rewrite);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                rewrite_scalar(base, rewrite);
            }
            for (condition, result) in when {
                rewrite_scalar(condition, rewrite);
                rewrite_scalar(result, rewrite);
            }
            if let Some(branch) = else_branch {
                rewrite_scalar(branch, rewrite);
            }
        }
        ScalarExpr::InSubquery { expr, .. } => rewrite_scalar(expr, rewrite),
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
    rewrite(expression);
}

/// Visit one scalar-expression tree in post-order and rewrite each node once.
///
/// Query-owned callers that need scope-sensitive rewriting can use this entry
/// point without duplicating the exhaustive [`ScalarExpr`] traversal.
pub fn rewrite_scalar_expression(
    expression: &mut ScalarExpr,
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    rewrite_scalar(expression, rewrite);
}

pub(super) fn rewrite_frame_bound(
    bound: &mut ScalarFrameBound,
    rewrite: &mut dyn FnMut(&mut ScalarExpr),
) {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            rewrite_scalar(expression, rewrite);
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
}
