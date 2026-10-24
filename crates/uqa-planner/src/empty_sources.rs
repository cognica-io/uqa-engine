//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prove that an already-analyzed relational input has no executable scan.

use uqa_core::Value;
use uqa_sql::{
    ast::JoinKind,
    plan::{ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan},
    ScalarExpr,
};

/// A folded qualification that cannot admit a row. LIMIT 0 and runtime row counts do not remove scan initialization.
fn rejects_rows(expression: Option<&ScalarExpr>) -> bool {
    matches!(
        expression,
        Some(
            ScalarExpr::Literal(Value::Bool(false) | Value::Null)
                | ScalarExpr::TypedLiteral {
                    value: Value::Bool(false) | Value::Null,
                    ..
                }
        )
    )
}

/// Whether a block's input can be replaced by an empty scan. Its output expressions and aggregation remain in the executable plan.
pub fn block_input_is_empty(block: &QueryBlockPlan) -> bool {
    (rejects_rows(block.r#where.as_ref()) && block.from.as_ref().is_none_or(discardable_outputs))
        || block.from.as_ref().is_some_and(source_is_empty)
}

fn passive_output(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Column(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::Literal(_)
            | ScalarExpr::TypedLiteral { .. }
    )
}

fn passive_slice(expression: Option<&ScalarExpr>) -> bool {
    match expression {
        None | Some(ScalarExpr::Literal(Value::Null)) => true,
        Some(
            ScalarExpr::Literal(Value::Int(value))
            | ScalarExpr::TypedLiteral {
                value: Value::Int(value),
                ..
            },
        ) => *value >= 0,
        _ => false,
    }
}

/// Function expressions in a surviving result projection still initialize even when its row filter is false. A scan proof must not discard that projection along with its input, or suppress LIMIT/OFFSET evaluation errors.
fn discardable_query_outputs(query: &QueryPlan) -> bool {
    query.ctes.is_empty()
        && matches!(&query.root, RelationalPlan::QueryBlock(block)
        if block.projections.iter().all(|p| passive_output(&p.expr))
            && block.group_by.iter().all(passive_output)
            && block.grouping_sets.iter().flatten().all(passive_output)
            && block.order_by.iter().all(|o| passive_output(&o.expr))
            && block.r#where.as_ref().is_none_or(passive_output)
            && block.having.as_ref().is_none_or(passive_output)
            && block.distinct_on.iter().all(passive_output)
            && block.windows.is_empty() && block.subqueries.is_empty()
        && passive_slice(block.limit.as_ref()) && passive_slice(block.offset.as_ref())
        && block.from.as_ref().is_none_or(discardable_outputs))
}

fn discardable_outputs(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Subquery { body, .. }
        | SourcePlan::Table {
            executable_query: Some(body),
            ..
        } => discardable_query_outputs(body),
        SourcePlan::Join { left, right, .. } => {
            discardable_outputs(left) && discardable_outputs(right)
        }
        SourcePlan::Values { rows, .. } => rows.iter().flatten().all(passive_output),
        SourcePlan::RetainedRows(_)
        | SourcePlan::Table { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => true,
    }
}

fn query_is_empty(query: &QueryPlan) -> bool {
    // CTE scheduling owns command effects and materialization. Do not erase their enclosing query when replacing a FROM source.
    if !query.ctes.is_empty() {
        return false;
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            // An ungrouped aggregate, or any empty grouping set, can produce a row from an empty input.
            let empty_group = matches!(block.compute, ComputePlan::Aggregate)
                && ((block.group_by.is_empty() && block.grouping_sets.is_empty())
                    || block.grouping_sets.iter().any(Vec::is_empty));
            !empty_group && block_input_is_empty(block) && discardable_query_outputs(query)
        }
        // PostgreSQL retains initialization at set-operation and VALUES boundaries even when their eventual result is empty.
        RelationalPlan::SetOp { .. } | RelationalPlan::Values { .. } => false,
    }
}

/// Prove emptiness from the current UnifiedPlan alone, without inspecting data, evaluating expressions, or using cardinality estimates.
pub fn source_is_empty(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Subquery { body, .. } => query_is_empty(body),
        SourcePlan::Table {
            executable_query: Some(body),
            ..
        } => query_is_empty(body),
        SourcePlan::Join {
            left,
            right,
            kind,
            on,
            ..
        } => match kind {
            JoinKind::Inner | JoinKind::Cross => {
                (rejects_rows(on.as_ref())
                    && discardable_outputs(left)
                    && discardable_outputs(right))
                    || (source_is_empty(left) && discardable_outputs(right))
                    || (source_is_empty(right) && discardable_outputs(left))
            }
            JoinKind::Left => source_is_empty(left),
            JoinKind::Right => source_is_empty(right),
            JoinKind::Full => source_is_empty(left) && source_is_empty(right),
        },
        SourcePlan::Values { rows, .. } => rows.is_empty(),
        SourcePlan::RetainedRows(_)
        | SourcePlan::Table { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => false,
    }
}

/// A false outer-join qualification removes only its matching input; preserved rows still need their original scan.
pub fn join_input_is_empty(join: &SourcePlan, right_input: bool) -> bool {
    let SourcePlan::Join {
        left,
        right,
        kind,
        on,
        ..
    } = join
    else {
        return false;
    };
    source_is_empty(if right_input { right } else { left })
        || (rejects_rows(on.as_ref())
            && matches!(
                (kind, right_input),
                (JoinKind::Left, true) | (JoinKind::Right, false)
            ))
}

#[cfg(test)]
mod tests;
