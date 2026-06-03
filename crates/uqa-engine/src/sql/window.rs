//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL window function evaluation.

use super::{
    aggregate_value, projection_columns, AggregateAccumulator, BTreeMap, Engine, Expr, OrderBy,
    Projection, ResultRow, SQLError, SQLParam, Value, WindowSpec,
};

pub(super) fn has_window(projections: &[Projection]) -> bool {
    projections
        .iter()
        .any(|p| matches!(p.expr, Expr::WindowCall { .. }))
}

pub(super) fn compute_window_columns(
    engine: &Engine,
    projections: &[Projection],
    rows: Vec<ResultRow>,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = rows;
    let labels = projection_columns(projections);
    for (idx, proj) in projections.iter().enumerate() {
        let Expr::WindowCall { name, args, spec } = &proj.expr else {
            continue;
        };
        let label = labels[idx].clone();
        let values = evaluate_window(engine, name, args, spec, &rows, params)?;
        for (row, value) in rows.iter_mut().zip(values) {
            row.insert(label.clone(), value);
        }
    }
    Ok(rows)
}

fn evaluate_window(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    spec: &WindowSpec,
    rows: &[ResultRow],
    params: &[SQLParam],
) -> Result<Vec<Value>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut partitions: BTreeMap<Vec<Value>, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
        let key: Vec<Value> = spec
            .partition_by
            .iter()
            .map(|e| uqa_sql::expr::eval(e, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        partitions.entry(key).or_default().push(i);
    }
    let mut output = vec![Value::Null; rows.len()];
    let lower = name.to_ascii_lowercase();
    for (_, indices) in partitions {
        let mut indexed: Vec<(usize, Vec<Value>)> = indices
            .into_iter()
            .map(|i| -> Result<_, SQLError> {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&rows[i]), params).with_engine(engine);
                let key: Vec<Value> = spec
                    .order_by
                    .iter()
                    .map(|o| uqa_sql::expr::eval(&o.expr, &ctx))
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
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params)
                                .with_engine(engine);
                        match uqa_sql::expr::eval(expr, &ctx)? {
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
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params)
                                .with_engine(engine);
                        uqa_sql::expr::eval(expr, &ctx)?
                    }
                };
                for (i, (orig, _)) in indexed.iter().enumerate() {
                    let target_idx = i as i64 + direction * offset_value;
                    let value = if target_idx < 0 || target_idx as usize >= indexed.len() {
                        default_value.clone()
                    } else {
                        let target_orig = indexed[target_idx as usize].0;
                        let ctx = uqa_sql::expr::EvalContext::new(Some(&rows[target_orig]), params)
                            .with_engine(engine);
                        uqa_sql::expr::eval(target_expr, &ctx)?
                    };
                    output[*orig] = value;
                }
            }
            "ntile" => {
                let n = match args.first() {
                    Some(expr) => {
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params)
                                .with_engine(engine);
                        match uqa_sql::expr::eval(expr, &ctx)? {
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
                    engine,
                    &lower,
                    args,
                    spec,
                    rows,
                    params,
                    &indexed,
                    &mut output,
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
    engine: &Engine,
    name: &str,
    args: &[Expr],
    spec: &WindowSpec,
    rows: &[ResultRow],
    params: &[SQLParam],
    indexed: &[(usize, Vec<Value>)],
    output: &mut [Value],
) -> Result<(), SQLError> {
    use uqa_sql::ast::{FrameBound, FrameMode};
    let arg_expr = args.first();
    let n = indexed.len();
    let materialized: Vec<Value> = match arg_expr {
        Some(expr) => indexed
            .iter()
            .map(|(orig, _)| {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&rows[*orig]), params).with_engine(engine);
                uqa_sql::expr::eval(expr, &ctx)
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
            let mut acc = AggregateAccumulator::default();
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
            FrameBound::UnboundedPreceding,
            FrameBound::CurrentRow,
        ),
    };
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
                    engine,
                )?,
                resolve_range_frame_index(
                    i,
                    n,
                    &order_keys,
                    &end_bound,
                    false,
                    rows,
                    params,
                    engine,
                )?,
            ),
            // GROUPS mode is rare; treat as ROWS (offset interpreted as
            // peer groups would require extra plumbing; matches the
            // fallback which also goes through `_resolve_frame_index`).
            FrameMode::Rows | FrameMode::Groups => (
                resolve_rows_frame_index(i, n, &start_bound, rows, params, engine, indexed)?,
                resolve_rows_frame_index(i, n, &end_bound, rows, params, engine, indexed)?,
            ),
        };
        let mut acc = AggregateAccumulator::default();
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

#[allow(clippy::too_many_arguments)]
fn resolve_rows_frame_index(
    current: usize,
    n: usize,
    bound: &uqa_sql::ast::FrameBound,
    rows: &[ResultRow],
    params: &[SQLParam],
    engine: &Engine,
    indexed: &[(usize, Vec<Value>)],
) -> Result<i64, SQLError> {
    use uqa_sql::ast::FrameBound;
    let n = n as i64;
    let cur = current as i64;
    Ok(match bound {
        FrameBound::UnboundedPreceding => 0,
        FrameBound::UnboundedFollowing => n - 1,
        FrameBound::CurrentRow => cur,
        FrameBound::Preceding(e) => {
            let off = eval_frame_offset(e, &rows[indexed[current].0], params, engine)?;
            (cur - off).max(0)
        }
        FrameBound::Following(e) => {
            let off = eval_frame_offset(e, &rows[indexed[current].0], params, engine)?;
            (cur + off).min(n - 1)
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve_range_frame_index(
    current: usize,
    n: usize,
    order_keys: &[Vec<Value>],
    bound: &uqa_sql::ast::FrameBound,
    is_start: bool,
    rows: &[ResultRow],
    params: &[SQLParam],
    engine: &Engine,
) -> Result<i64, SQLError> {
    use uqa_sql::ast::FrameBound;
    let key_at = |idx: usize| -> Option<&Value> { order_keys.get(idx).and_then(|k| k.first()) };
    Ok(match bound {
        FrameBound::UnboundedPreceding => 0,
        FrameBound::UnboundedFollowing => (n as i64) - 1,
        FrameBound::CurrentRow => {
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
        FrameBound::Preceding(e) | FrameBound::Following(e) => {
            let off = eval_frame_offset(e, &rows[current], params, engine)?;
            let cur_val = match key_at(current) {
                Some(Value::Int(n)) => *n as f64,
                Some(Value::Float(f)) => *f,
                _ => {
                    return Ok(if matches!(bound, FrameBound::Preceding(_)) {
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
            let target = if matches!(bound, FrameBound::Preceding(_)) {
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
    expr: &Expr,
    row: &ResultRow,
    params: &[SQLParam],
    engine: &Engine,
) -> Result<i64, SQLError> {
    let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
    match uqa_sql::expr::eval(expr, &ctx)? {
        Value::Int(n) => Ok(n),
        Value::Float(f) => Ok(f as i64),
        other => Err(SQLError::TypeMismatch(format!(
            "frame offset must be numeric, got {other:?}"
        ))),
    }
}

fn sort_keys(a: &[Value], b: &[Value], order: &[OrderBy]) -> std::cmp::Ordering {
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
