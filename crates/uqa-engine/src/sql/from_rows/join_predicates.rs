//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join conjunct analysis, side selection, and null padding.

use super::{
    eval_scalar, null_row_for, Engine, ResultRow, SQLError, SQLParam, ScalarEvalContext,
    ScalarExpr, ScalarSubqueryRunner, SourcePlan, Value,
};

pub(in crate::sql) fn join_conjuncts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    match expr {
        ScalarExpr::And(items) => {
            let mut conjuncts = Vec::with_capacity(items.len());
            for item in items {
                conjuncts.extend(join_conjuncts(item));
            }
            conjuncts
        }
        _ => vec![expr],
    }
}

/// Pick which expression evaluates over the left side and which over
/// the right by sampling the first row of each side. Returns
/// `(left_key_expr, right_key_expr)` when one direction works,
/// `None` when the predicate isn't separable across sides.
pub(in crate::sql) fn decide_join_sides<'a>(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    lhs: &'a ScalarExpr,
    rhs: &'a ScalarExpr,
    params: &[SQLParam],
) -> Option<(&'a ScalarExpr, &'a ScalarExpr)> {
    if left_rows.is_empty() || right_rows.is_empty() {
        return None;
    }
    let l_sample = &left_rows[0];
    let r_sample = &right_rows[0];
    let lhs_on_left = eval_yields_value(eval_hook, subquery_runner, l_sample, lhs, params);
    let rhs_on_right = eval_yields_value(eval_hook, subquery_runner, r_sample, rhs, params);
    if lhs_on_left && rhs_on_right {
        return Some((lhs, rhs));
    }
    let rhs_on_left = eval_yields_value(eval_hook, subquery_runner, l_sample, rhs, params);
    let lhs_on_right = eval_yields_value(eval_hook, subquery_runner, r_sample, lhs, params);
    if rhs_on_left && lhs_on_right {
        return Some((rhs, lhs));
    }
    None
}

pub(in crate::sql) fn eval_yields_value(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    row: &ResultRow,
    expr: &ScalarExpr,
    params: &[SQLParam],
) -> bool {
    let ctx = ScalarEvalContext::new(Some(row), params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(subquery_runner);
    matches!(eval_scalar(expr, &ctx), Ok(v) if v != uqa_core::Value::Null)
}

pub(in crate::sql) fn pad_nulls_for_from(
    row: &mut ResultRow,
    from: &SourcePlan,
    engine: &Engine,
) -> Result<(), SQLError> {
    let mut tables = Vec::new();
    from.collect_tables(&mut tables);
    for (name, alias) in &tables {
        let null_keys = null_row_for(name, alias.as_deref(), engine)?;
        for (k, v) in null_keys {
            row.entry(k).or_insert(v);
        }
    }
    Ok(())
}

pub(in crate::sql) fn join_schema_sample(columns: &[String]) -> ResultRow {
    columns
        .iter()
        .map(|column| (column.clone(), Value::Int(1)))
        .collect()
}
