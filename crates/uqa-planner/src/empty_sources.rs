//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prove that an already-analyzed relational input has no executable scan.

use uqa_core::Value;
use uqa_sql::{
    ast::{JoinKind, SetOpKind},
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

/// Whether the recursive scan owns the plain target list after pulling up eligible one-row inputs.
pub fn work_table_projection(
    block: &QueryBlockPlan,
    name: &str,
    routines: &dyn uqa_sql::routines::RoutineResolution,
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, uqa_sql::SQLError> {
    if !matches!(block.compute, ComputePlan::Project) || block_input_is_empty(block) {
        return Ok(false);
    }
    let foldable = |expression: &ScalarExpr| {
        Ok(!uqa_sql::semantics::volatility::expr_contains_volatile_function_with_control(volatility, expression, control)?
            && !uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically_with_control(routines, expression, control)?)
    };
    match &block.from {
        Some(source) => work_table_scan(source, name, &foldable),
        None => Ok(false),
    }
}

fn work_table_scan(
    source: &SourcePlan,
    name: &str,
    foldable: &impl Fn(&ScalarExpr) -> Result<bool, uqa_sql::SQLError>,
) -> Result<bool, uqa_sql::SQLError> {
    Ok(match source {
        SourcePlan::Table {
            name: reference,
            executable_query: None,
            ..
        } => uqa_core::RelationIdentity::borrowed_reference(reference).is_some_and(|reference| {
            reference.schema.is_none() && reference.name == uqa_core::RelationName::decoded(name)
        }),
        SourcePlan::Join {
            kind: JoinKind::Inner | JoinKind::Cross,
            left,
            right,
            ..
        } => {
            (work_table_scan(left, name, foldable)? && removable_result(right, foldable)?)
                || (work_table_scan(right, name, foldable)? && removable_result(left, foldable)?)
        }
        _ => false,
    })
}

/// A one-row Result can disappear into the other join input only if pulling up its projection preserves the number of rows and every volatile evaluation.
fn removable_result(
    source: &SourcePlan,
    foldable: &impl Fn(&ScalarExpr) -> Result<bool, uqa_sql::SQLError>,
) -> Result<bool, uqa_sql::SQLError> {
    let rows = match source {
        SourcePlan::Values { rows, .. } => rows,
        SourcePlan::Subquery { body, .. } if body.ctes.is_empty() => match &body.root {
            RelationalPlan::Values { rows, .. } => rows,
            RelationalPlan::QueryBlock(block) => {
                if !matches!(block.compute, ComputePlan::Project)
                    || block.from.is_some()
                    || rejects_rows(block.r#where.as_ref())
                    || block.having.is_some()
                    || block.limit.is_some()
                    || block.offset.is_some()
                    || block.distinct
                    || !block.distinct_on.is_empty()
                    || !block.group_by.is_empty()
                    || !block.grouping_sets.is_empty()
                    || !block.windows.is_empty()
                    || !block.order_by.is_empty()
                    || !block.locking.is_empty()
                {
                    return Ok(false);
                }
                for projection in &block.projections {
                    if !foldable(&projection.expr)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            RelationalPlan::SetOp { .. } => return Ok(false),
        },
        _ => return Ok(false),
    };
    let [row] = rows.as_slice() else {
        return Ok(false);
    };
    for value in row {
        if !foldable(value)? {
            return Ok(false);
        }
    }
    Ok(true)
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
    if !query.ctes.is_empty() {
        return false;
    }
    match &query.root {
        RelationalPlan::SetOp { .. } => query_is_empty(query),
        RelationalPlan::QueryBlock(block) => {
            block.projections.iter().all(|p| passive_output(&p.expr))
                && block.group_by.iter().all(passive_output)
                && block.grouping_sets.iter().flatten().all(passive_output)
                && block.order_by.iter().all(|o| passive_output(&o.expr))
                && block.r#where.as_ref().is_none_or(passive_output)
                && block.having.as_ref().is_none_or(passive_output)
                && block.distinct_on.iter().all(passive_output)
                && block.windows.is_empty()
                && block.subqueries.is_empty()
                && passive_slice(block.limit.as_ref())
                && passive_slice(block.offset.as_ref())
                && block.from.as_ref().is_none_or(discardable_outputs)
        }
        RelationalPlan::Values { .. } => false,
    }
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
        RelationalPlan::SetOp {
            kind: SetOpKind::Union,
            all: true,
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } if order_by.is_empty() && limit.is_none() && offset.is_none() => {
            // A flattened append of dummy relation inputs disappears. A source-free Result, coercing append, duplicate elimination or another set operator retains its executor boundary even when its result is empty.
            let same_types = query
                .analyzed_output
                .as_ref()
                .zip(left.analyzed_output.as_ref())
                .zip(right.analyzed_output.as_ref())
                .is_some_and(|((output, left), right)| {
                    output.column_types() == left.column_types()
                        && output.column_types() == right.column_types()
                });
            same_types && empty_append_arm(left) && empty_append_arm(right)
        }
        RelationalPlan::SetOp { .. } | RelationalPlan::Values { .. } => false,
    }
}

fn empty_append_arm(query: &QueryPlan) -> bool {
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            block.from.is_some()
                && matches!(block.compute, ComputePlan::Project)
                && block.group_by.is_empty()
                && block.grouping_sets.is_empty()
                && block.order_by.is_empty()
                && !block.distinct
                && block.distinct_on.is_empty()
                && block.limit.is_none()
                && block.offset.is_none()
                && query_is_empty(query)
        }
        RelationalPlan::SetOp { .. } => query_is_empty(query),
        RelationalPlan::Values { .. } => false,
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
