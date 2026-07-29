//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL window function evaluation.

use uqa_execution::{
    eval_scalar, ScalarEvalContext, ScalarExpr, ScalarFrameBound, ScalarOrder,
    ScalarSubqueryRunner, ScalarWindowSpec,
};
use uqa_planner::ProjectionPlan;

use super::scalar::PlanSubqueryArena;
use super::{
    aggregate_value, projection_columns, AggregateAccumulator, BTreeMap, CteScope, Engine,
    ResultRow, SQLError, SQLParam, ScopedEngineHook, Value,
};

pub(super) struct WindowProjectionResult {
    pub rows: Vec<ResultRow>,
    pub projections: Vec<ProjectionPlan>,
}

#[derive(Clone)]
struct WindowSlot {
    key: String,
    name: String,
    args: Vec<ScalarExpr>,
    spec: ScalarWindowSpec,
}

pub(super) fn has_window(projections: &[ProjectionPlan]) -> bool {
    projections.iter().any(|p| expr_has_window(&p.expr))
}

pub(super) fn compute_window_columns(
    engine: &Engine,
    projections: &[ProjectionPlan],
    rows: Vec<ResultRow>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<WindowProjectionResult, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let subquery_arena = PlanSubqueryArena::new(&ctes.scalar_subqueries, Some(&hook));
    let mut rows = rows;
    let labels = projection_columns(projections);
    let mut slots = Vec::new();
    let mut rewritten = Vec::with_capacity(projections.len());
    for (idx, projection) in projections.iter().enumerate() {
        let mut counter = 0usize;
        let (expr, changed) = rewrite_window_expr(&projection.expr, idx, &mut counter, &mut slots);
        let mut projection = projection.clone();
        projection.expr = expr;
        if changed && projection.alias.is_none() {
            projection.alias = Some(labels[idx].clone());
        }
        rewritten.push(projection);
    }
    let output_order_spec = slots
        .iter()
        .rev()
        .find(|slot| !slot.spec.order_by.is_empty() || !slot.spec.partition_by.is_empty())
        .map(|slot| slot.spec.clone());
    for slot in slots {
        let values = evaluate_window(
            &slot.name,
            &slot.args,
            &slot.spec,
            &rows,
            params,
            &hook,
            &subquery_arena,
        )?;
        for (row, value) in rows.iter_mut().zip(values) {
            row.insert(slot.key.clone(), value);
        }
    }
    // PostgreSQL emits window-query rows in the order of the final
    // windowing sort (partition keys, then the window ORDER BY) when
    // no outer ORDER BY overrides it; an outer ORDER BY re-sorts later.
    if let Some(spec) = output_order_spec {
        let mut keyed: Vec<(Vec<Value>, Vec<Value>, ResultRow)> = rows
            .into_iter()
            .map(|row| -> Result<_, SQLError> {
                let ctx = ScalarEvalContext::new(Some(&row), params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&subquery_arena);
                let partition: Vec<Value> = spec
                    .partition_by
                    .iter()
                    .map(|e| eval_scalar(e, &ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                let order: Vec<Value> = spec
                    .order_by
                    .iter()
                    .map(|o| eval_scalar(&o.expr, &ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((partition, order, row))
            })
            .collect::<Result<Vec<_>, _>>()?;
        keyed.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| sort_keys(&a.1, &b.1, &spec.order_by))
        });
        rows = keyed.into_iter().map(|(_, _, row)| row).collect();
    }
    Ok(WindowProjectionResult {
        rows,
        projections: rewritten,
    })
}

fn expr_has_window(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::WindowCall { .. } => true,
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_has_window)
                || order_by.iter().any(|order| expr_has_window(&order.expr))
                || filter.as_ref().is_some_and(|expr| expr_has_window(expr))
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_has_window)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => expr_has_window(lhs) || expr_has_window(rhs),
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_has_window(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_has_window(expr) || expr_has_window(low) || expr_has_window(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_has_window(expr) || list.iter().any(expr_has_window)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref().is_some_and(|expr| expr_has_window(expr))
                || when
                    .iter()
                    .any(|(cond, result)| expr_has_window(cond) || expr_has_window(result))
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_has_window(expr))
        }
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}

fn rewrite_window_expr(
    expr: &ScalarExpr,
    projection_index: usize,
    counter: &mut usize,
    slots: &mut Vec<WindowSlot>,
) -> (ScalarExpr, bool) {
    match expr {
        ScalarExpr::WindowCall { name, args, spec } => {
            let key = format!("__window_{projection_index}_{}", *counter);
            *counter += 1;
            slots.push(WindowSlot {
                key: key.clone(),
                name: name.clone(),
                args: args.clone(),
                spec: spec.clone(),
            });
            (ScalarExpr::Column(key), true)
        }
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => {
            let (args, args_changed) = rewrite_window_exprs(args, projection_index, counter, slots);
            let (order_by, order_changed) =
                rewrite_window_order_by(order_by, projection_index, counter, slots);
            let (filter, filter_changed) = match filter {
                Some(expr) => {
                    let (expr, changed) =
                        rewrite_window_expr(expr, projection_index, counter, slots);
                    (Some(Box::new(expr)), changed)
                }
                None => (None, false),
            };
            (
                ScalarExpr::Func {
                    name: name.clone(),
                    args,
                    distinct: *distinct,
                    order_by,
                    filter,
                },
                args_changed || order_changed || filter_changed,
            )
        }
        ScalarExpr::Array(items) => {
            let (items, changed) = rewrite_window_exprs(items, projection_index, counter, slots);
            (ScalarExpr::Array(items), changed)
        }
        ScalarExpr::Binary { op, lhs, rhs } => {
            let (lhs, lhs_changed) = rewrite_window_expr(lhs, projection_index, counter, slots);
            let (rhs, rhs_changed) = rewrite_window_expr(rhs, projection_index, counter, slots);
            (
                ScalarExpr::Binary {
                    op: *op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                lhs_changed || rhs_changed,
            )
        }
        ScalarExpr::Not(inner) => {
            let (inner, changed) = rewrite_window_expr(inner, projection_index, counter, slots);
            (ScalarExpr::Not(Box::new(inner)), changed)
        }
        ScalarExpr::And(items) => {
            let (items, changed) = rewrite_window_exprs(items, projection_index, counter, slots);
            (ScalarExpr::And(items), changed)
        }
        ScalarExpr::Or(items) => {
            let (items, changed) = rewrite_window_exprs(items, projection_index, counter, slots);
            (ScalarExpr::Or(items), changed)
        }
        ScalarExpr::IsNull { expr, negated } => {
            let (expr, changed) = rewrite_window_expr(expr, projection_index, counter, slots);
            (
                ScalarExpr::IsNull {
                    expr: Box::new(expr),
                    negated: *negated,
                },
                changed,
            )
        }
        ScalarExpr::Between { expr, low, high } => {
            let (expr, expr_changed) = rewrite_window_expr(expr, projection_index, counter, slots);
            let (low, low_changed) = rewrite_window_expr(low, projection_index, counter, slots);
            let (high, high_changed) = rewrite_window_expr(high, projection_index, counter, slots);
            (
                ScalarExpr::Between {
                    expr: Box::new(expr),
                    low: Box::new(low),
                    high: Box::new(high),
                },
                expr_changed || low_changed || high_changed,
            )
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => {
            let (expr, expr_changed) = rewrite_window_expr(expr, projection_index, counter, slots);
            let (list, list_changed) = rewrite_window_exprs(list, projection_index, counter, slots);
            (
                ScalarExpr::InList {
                    expr: Box::new(expr),
                    list,
                    negated: *negated,
                },
                expr_changed || list_changed,
            )
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            let (base, base_changed) = match base {
                Some(expr) => {
                    let (expr, changed) =
                        rewrite_window_expr(expr, projection_index, counter, slots);
                    (Some(Box::new(expr)), changed)
                }
                None => (None, false),
            };
            let mut changed = base_changed;
            let mut rewritten_when = Vec::with_capacity(when.len());
            for (cond, result) in when {
                let (cond, cond_changed) =
                    rewrite_window_expr(cond, projection_index, counter, slots);
                let (result, result_changed) =
                    rewrite_window_expr(result, projection_index, counter, slots);
                changed |= cond_changed || result_changed;
                rewritten_when.push((cond, result));
            }
            let (else_branch, else_changed) = match else_branch {
                Some(expr) => {
                    let (expr, changed) =
                        rewrite_window_expr(expr, projection_index, counter, slots);
                    (Some(Box::new(expr)), changed)
                }
                None => (None, false),
            };
            (
                ScalarExpr::Case {
                    base,
                    when: rewritten_when,
                    else_branch,
                },
                changed || else_changed,
            )
        }
        ScalarExpr::Cast { expr, ty } => {
            let (expr, changed) = rewrite_window_expr(expr, projection_index, counter, slots);
            (
                ScalarExpr::Cast {
                    expr: Box::new(expr),
                    ty: ty.clone(),
                },
                changed,
            )
        }
        _ => (expr.clone(), false),
    }
}

fn rewrite_window_exprs(
    exprs: &[ScalarExpr],
    projection_index: usize,
    counter: &mut usize,
    slots: &mut Vec<WindowSlot>,
) -> (Vec<ScalarExpr>, bool) {
    let mut changed = false;
    let rewritten = exprs
        .iter()
        .map(|expr| {
            let (expr, expr_changed) = rewrite_window_expr(expr, projection_index, counter, slots);
            changed |= expr_changed;
            expr
        })
        .collect();
    (rewritten, changed)
}

fn rewrite_window_order_by(
    order_by: &[ScalarOrder],
    projection_index: usize,
    counter: &mut usize,
    slots: &mut Vec<WindowSlot>,
) -> (Vec<ScalarOrder>, bool) {
    let mut changed = false;
    let rewritten = order_by
        .iter()
        .map(|order| {
            let (expr, expr_changed) =
                rewrite_window_expr(&order.expr, projection_index, counter, slots);
            changed |= expr_changed;
            ScalarOrder {
                expr,
                descending: order.descending,
                nulls: order.nulls,
            }
        })
        .collect();
    (rewritten, changed)
}

fn evaluate_window(
    name: &str,
    args: &[ScalarExpr],
    spec: &ScalarWindowSpec,
    rows: &[ResultRow],
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<Vec<Value>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut partitions: BTreeMap<Vec<Value>, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let ctx = ScalarEvalContext::new(Some(row), params)
            .with_function_hook(eval_hook)
            .with_subquery_runner(subquery_runner);
        let key: Vec<Value> = spec
            .partition_by
            .iter()
            .map(|e| eval_scalar(e, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        partitions.entry(key).or_default().push(i);
    }
    let mut output = vec![Value::Null; rows.len()];
    let lower = name.to_ascii_lowercase();
    for (_, indices) in partitions {
        let mut indexed: Vec<(usize, Vec<Value>)> = indices
            .into_iter()
            .map(|i| -> Result<_, SQLError> {
                let ctx = ScalarEvalContext::new(Some(&rows[i]), params)
                    .with_function_hook(eval_hook)
                    .with_subquery_runner(subquery_runner);
                let key: Vec<Value> = spec
                    .order_by
                    .iter()
                    .map(|o| eval_scalar(&o.expr, &ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((i, key))
            })
            .collect::<Result<Vec<_>, _>>()?;
        indexed.sort_by(|a, b| sort_keys(&a.1, &b.1, &spec.order_by));

        match lower.as_str() {
            "row_number" => {
                for (rank, (orig, _)) in indexed.iter().enumerate() {
                    output[*orig] = Value::Int((rank + 1) as i64);
                }
            }
            "rank" => {
                let mut last_key: Option<Vec<Value>> = None;
                let mut last_rank = 0i64;
                for (i, (orig, key)) in indexed.iter().enumerate() {
                    let rank = if last_key.as_ref() == Some(key) {
                        last_rank
                    } else {
                        last_key = Some(key.clone());
                        last_rank = (i + 1) as i64;
                        last_rank
                    };
                    output[*orig] = Value::Int(rank);
                }
            }
            "dense_rank" => {
                let mut last_key: Option<Vec<Value>> = None;
                let mut last_rank = 0i64;
                for (orig, key) in &indexed {
                    if last_key.as_ref() != Some(key) {
                        last_rank += 1;
                        last_key = Some(key.clone());
                    }
                    output[*orig] = Value::Int(last_rank);
                }
            }
            "lag" | "lead" => {
                let direction: i64 = if lower == "lag" { -1 } else { 1 };
                let target_expr = args.first().ok_or_else(|| SQLError::BadArity {
                    name: lower.clone(),
                    expected: ">=1".into(),
                    actual: 0,
                })?;
                let offset_value = match args.get(1) {
                    None => 1i64,
                    Some(expr) => {
                        let ctx = ScalarEvalContext::new(Some(&rows[indexed[0].0]), params)
                            .with_function_hook(eval_hook)
                            .with_subquery_runner(subquery_runner);
                        match eval_scalar(expr, &ctx)? {
                            Value::Int(n) => n,
                            other => {
                                return Err(SQLError::TypeMismatch(format!(
                                    "lag/lead offset must be integer, got {other:?}"
                                )));
                            }
                        }
                    }
                };
                let default_value = match args.get(2) {
                    None => Value::Null,
                    Some(expr) => {
                        let ctx = ScalarEvalContext::new(Some(&rows[indexed[0].0]), params)
                            .with_function_hook(eval_hook)
                            .with_subquery_runner(subquery_runner);
                        eval_scalar(expr, &ctx)?
                    }
                };
                for (i, (orig, _)) in indexed.iter().enumerate() {
                    let target_idx = i as i64 + direction * offset_value;
                    let value = if target_idx < 0 || target_idx as usize >= indexed.len() {
                        default_value.clone()
                    } else {
                        let target_orig = indexed[target_idx as usize].0;
                        let ctx = ScalarEvalContext::new(Some(&rows[target_orig]), params)
                            .with_function_hook(eval_hook)
                            .with_subquery_runner(subquery_runner);
                        eval_scalar(target_expr, &ctx)?
                    };
                    output[*orig] = value;
                }
            }
            "ntile" => {
                let n = match args.first() {
                    Some(expr) => {
                        let ctx = ScalarEvalContext::new(Some(&rows[indexed[0].0]), params)
                            .with_function_hook(eval_hook)
                            .with_subquery_runner(subquery_runner);
                        match eval_scalar(expr, &ctx)? {
                            Value::Int(n) if n > 0 => n,
                            other => {
                                return Err(SQLError::TypeMismatch(format!(
                                    "ntile bucket count must be positive integer, got {other:?}"
                                )));
                            }
                        }
                    }
                    None => {
                        return Err(SQLError::BadArity {
                            name: "ntile".into(),
                            expected: "1".into(),
                            actual: 0,
                        });
                    }
                };
                let len = indexed.len() as i64;
                let base = len / n;
                let extra = len % n;
                let mut bucket = 1i64;
                let mut consumed_in_bucket = 0i64;
                let mut bucket_size = if 1 <= extra { base + 1 } else { base };
                for (orig, _) in &indexed {
                    if bucket_size == 0 {
                        output[*orig] = Value::Int(bucket);
                        bucket += 1;
                        continue;
                    }
                    output[*orig] = Value::Int(bucket);
                    consumed_in_bucket += 1;
                    if consumed_in_bucket == bucket_size {
                        bucket += 1;
                        consumed_in_bucket = 0;
                        bucket_size = if bucket <= extra { base + 1 } else { base };
                    }
                }
            }
            "sum" | "count" | "avg" | "min" | "max" => {
                evaluate_window_aggregate(
                    &lower,
                    args,
                    spec,
                    rows,
                    params,
                    &indexed,
                    &mut output,
                    eval_hook,
                    subquery_runner,
                )?;
            }
            other => {
                return Err(SQLError::UnknownFunction(format!(
                    "window function `{other}` is not supported"
                )));
            }
        }
    }
    Ok(output)
}

/// Evaluate an aggregate window function (SUM/COUNT/AVG/MIN/MAX) over
/// each row's frame. Matches UQA behavior for `_compute_framed_aggregate` in
/// uqa/execution/relational.py.
#[allow(clippy::too_many_arguments)]
fn evaluate_window_aggregate(
    name: &str,
    args: &[ScalarExpr],
    spec: &ScalarWindowSpec,
    rows: &[ResultRow],
    params: &[SQLParam],
    indexed: &[(usize, Vec<Value>)],
    output: &mut [Value],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<(), SQLError> {
    use uqa_sql::ast::FrameMode;
    let arg_expr = args.first();
    let n = indexed.len();
    let materialized: Vec<Value> = match arg_expr {
        Some(expr) => indexed
            .iter()
            .map(|(orig, _)| {
                let ctx = ScalarEvalContext::new(Some(&rows[*orig]), params)
                    .with_function_hook(eval_hook)
                    .with_subquery_runner(subquery_runner);
                eval_scalar(expr, &ctx)
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![Value::Int(1); n],
    };
    let order_keys: Vec<Vec<Value>> = indexed.iter().map(|(_, k)| k.clone()).collect();
    let (mode, start_bound, end_bound) = match &spec.frame {
        Some(f) => (f.mode, f.start.clone(), f.end.clone()),
        None if spec.order_by.is_empty() => {
            // No ORDER BY and no explicit frame: aggregate over the
            // whole partition.
            let mut acc = AggregateAccumulator::builtin(name);
            for v in &materialized {
                acc.observe(v)?;
            }
            let result = aggregate_value(name, &acc)?;
            for (orig, _) in indexed {
                output[*orig] = result.clone();
            }
            return Ok(());
        }
        None => (
            FrameMode::Rows,
            ScalarFrameBound::UnboundedPreceding,
            ScalarFrameBound::CurrentRow,
        ),
    };
    if matches!(mode, FrameMode::Rows)
        && matches!(start_bound, ScalarFrameBound::UnboundedPreceding)
        && matches!(end_bound, ScalarFrameBound::CurrentRow)
    {
        if let Some(values) = prefix_numeric_window_values(name, &materialized) {
            for ((orig, _), value) in indexed.iter().zip(values) {
                output[*orig] = value;
            }
            return Ok(());
        }
        let mut acc = AggregateAccumulator::builtin(name);
        for (i, (orig, _)) in indexed.iter().enumerate() {
            acc.observe(&materialized[i])?;
            output[*orig] = aggregate_value(name, &acc)?;
        }
        return Ok(());
    }
    for (i, (orig, _)) in indexed.iter().enumerate() {
        let (start, end) = match mode {
            FrameMode::Range => (
                resolve_range_frame_index(
                    i,
                    n,
                    &order_keys,
                    &start_bound,
                    /* is_start = */ true,
                    rows,
                    params,
                    eval_hook,
                    subquery_runner,
                )?,
                resolve_range_frame_index(
                    i,
                    n,
                    &order_keys,
                    &end_bound,
                    false,
                    rows,
                    params,
                    eval_hook,
                    subquery_runner,
                )?,
            ),
            // GROUPS mode is rare; treat as ROWS (offset interpreted as
            // peer groups would require extra plumbing; matches the
            // fallback which also goes through `_resolve_frame_index`).
            FrameMode::Rows | FrameMode::Groups => (
                resolve_rows_frame_index(
                    i,
                    n,
                    &start_bound,
                    rows,
                    params,
                    indexed,
                    eval_hook,
                    subquery_runner,
                )?,
                resolve_rows_frame_index(
                    i,
                    n,
                    &end_bound,
                    rows,
                    params,
                    indexed,
                    eval_hook,
                    subquery_runner,
                )?,
            ),
        };
        let mut acc = AggregateAccumulator::builtin(name);
        if start <= end && start < n as i64 && end >= 0 {
            let lo = start.max(0) as usize;
            let hi = (end as usize).min(n.saturating_sub(1));
            for v in &materialized[lo..=hi] {
                acc.observe(v)?;
            }
        }
        output[*orig] = aggregate_value(name, &acc)?;
    }
    Ok(())
}

fn prefix_numeric_window_values(name: &str, values: &[Value]) -> Option<Vec<Value>> {
    match name {
        "count" => {
            let mut count = 0i64;
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                if !matches!(value, Value::Null) {
                    count += 1;
                }
                out.push(Value::Int(count));
            }
            Some(out)
        }
        "sum" => {
            let mut count = 0i64;
            let mut sum = 0.0f64;
            let mut all_int = true;
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                match value {
                    Value::Null => {}
                    Value::Int(n) => {
                        count += 1;
                        sum += *n as f64;
                    }
                    Value::Float(f) => {
                        count += 1;
                        sum += *f;
                        all_int = false;
                    }
                    _ => return None,
                }
                if count == 0 {
                    out.push(Value::Null);
                } else if all_int {
                    out.push(Value::Int(sum as i64));
                } else {
                    out.push(Value::Float(sum));
                }
            }
            Some(out)
        }
        "avg" => {
            let mut count = 0i64;
            let mut sum = 0.0f64;
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                match value {
                    Value::Null => {}
                    Value::Int(n) => {
                        count += 1;
                        sum += *n as f64;
                    }
                    Value::Float(f) => {
                        count += 1;
                        sum += *f;
                    }
                    _ => return None,
                }
                if count == 0 {
                    out.push(Value::Null);
                } else {
                    out.push(Value::Float(sum / count as f64));
                }
            }
            Some(out)
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_rows_frame_index(
    current: usize,
    n: usize,
    bound: &ScalarFrameBound,
    rows: &[ResultRow],
    params: &[SQLParam],
    indexed: &[(usize, Vec<Value>)],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<i64, SQLError> {
    let n = n as i64;
    let cur = current as i64;
    Ok(match bound {
        ScalarFrameBound::UnboundedPreceding => 0,
        ScalarFrameBound::UnboundedFollowing => n - 1,
        ScalarFrameBound::CurrentRow => cur,
        ScalarFrameBound::Preceding(e) => {
            let off = eval_frame_offset(
                e,
                &rows[indexed[current].0],
                params,
                eval_hook,
                subquery_runner,
            )?;
            (cur - off).max(0)
        }
        ScalarFrameBound::Following(e) => {
            let off = eval_frame_offset(
                e,
                &rows[indexed[current].0],
                params,
                eval_hook,
                subquery_runner,
            )?;
            (cur + off).min(n - 1)
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve_range_frame_index(
    current: usize,
    n: usize,
    order_keys: &[Vec<Value>],
    bound: &ScalarFrameBound,
    is_start: bool,
    rows: &[ResultRow],
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<i64, SQLError> {
    let key_at = |idx: usize| -> Option<&Value> { order_keys.get(idx).and_then(|k| k.first()) };
    Ok(match bound {
        ScalarFrameBound::UnboundedPreceding => 0,
        ScalarFrameBound::UnboundedFollowing => (n as i64) - 1,
        ScalarFrameBound::CurrentRow => {
            let cur_val = key_at(current).cloned().unwrap_or(Value::Null);
            if is_start {
                let mut idx = current;
                while idx > 0 && matches!(key_at(idx - 1), Some(v) if v == &cur_val) {
                    idx -= 1;
                }
                idx as i64
            } else {
                let mut idx = current;
                while idx + 1 < n && matches!(key_at(idx + 1), Some(v) if v == &cur_val) {
                    idx += 1;
                }
                idx as i64
            }
        }
        ScalarFrameBound::Preceding(e) | ScalarFrameBound::Following(e) => {
            let off = eval_frame_offset(e, &rows[current], params, eval_hook, subquery_runner)?;
            let cur_val = match key_at(current) {
                Some(Value::Int(n)) => *n as f64,
                Some(Value::Float(f)) => *f,
                _ => {
                    return Ok(if matches!(bound, ScalarFrameBound::Preceding(_)) {
                        if is_start {
                            0
                        } else {
                            current as i64
                        }
                    } else if is_start {
                        current as i64
                    } else {
                        (n as i64) - 1
                    });
                }
            };
            let target = if matches!(bound, ScalarFrameBound::Preceding(_)) {
                cur_val - off as f64
            } else {
                cur_val + off as f64
            };
            if is_start {
                let mut idx: i64 = -1;
                for i in 0..n {
                    let val = match key_at(i) {
                        Some(Value::Int(n)) => *n as f64,
                        Some(Value::Float(f)) => *f,
                        _ => continue,
                    };
                    if val >= target {
                        idx = i as i64;
                        break;
                    }
                }
                if idx < 0 {
                    n as i64
                } else {
                    idx
                }
            } else {
                let mut idx: i64 = -1;
                for i in 0..n {
                    let val = match key_at(i) {
                        Some(Value::Int(n)) => *n as f64,
                        Some(Value::Float(f)) => *f,
                        _ => continue,
                    };
                    if val <= target {
                        idx = i as i64;
                    } else {
                        break;
                    }
                }
                idx
            }
        }
    })
}

fn eval_frame_offset(
    expr: &ScalarExpr,
    row: &ResultRow,
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<i64, SQLError> {
    let ctx = ScalarEvalContext::new(Some(row), params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(subquery_runner);
    match eval_scalar(expr, &ctx)? {
        Value::Int(n) => Ok(n),
        Value::Float(f) => Ok(f as i64),
        other => Err(SQLError::TypeMismatch(format!(
            "frame offset must be numeric, got {other:?}"
        ))),
    }
}

fn sort_keys(a: &[Value], b: &[Value], order: &[ScalarOrder]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    use uqa_sql::ast::NullsOrder;
    for (i, (av, bv)) in a.iter().zip(b.iter()).enumerate() {
        let descending = order.get(i).is_some_and(|o| o.descending);
        // Resolve NULLS FIRST/LAST. Default mirrors PostgreSQL: ASC maps
        // to NULLS LAST, DESC maps to NULLS FIRST.
        let nulls_first = match order.get(i).and_then(|o| o.nulls) {
            Some(NullsOrder::First) => true,
            Some(NullsOrder::Last) => false,
            None => descending,
        };
        let a_null = matches!(av, Value::Null);
        let b_null = matches!(bv, Value::Null);
        if a_null || b_null {
            let null_cmp = match (a_null, b_null) {
                (true, true) => Ordering::Equal,
                (true, false) => {
                    if nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (false, true) => {
                    if nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                (false, false) => unreachable!(),
            };
            if null_cmp != Ordering::Equal {
                return null_cmp;
            }
            continue;
        }
        let mut cmp = compare_values(av, bv);
        if descending {
            cmp = cmp.reverse();
        }
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    Ordering::Equal
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Temporal(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Str(y)) => x
            .parse_same_kind(y)
            .map_or(Ordering::Equal, |parsed| x.cmp(&parsed)),
        (Value::Str(x), Value::Temporal(y)) => y
            .parse_same_kind(x)
            .map_or(Ordering::Equal, |parsed| parsed.cmp(y)),
        _ => Ordering::Equal,
    }
}
