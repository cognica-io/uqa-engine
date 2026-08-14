//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join conjunct analysis, side selection, and null padding.

use super::{
    eval_scalar, ResultRow, SQLParam, ScalarEvalContext, ScalarExpr, ScalarSubqueryRunner, Value,
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

/// Bind a join key's column lookup to the exact physical schema key whenever
/// the side schema identifies it unambiguously. Parsed SQL commonly carries
/// bare column names (for example `l_orderkey`) while joined rows store
/// qualified keys (`lineitem.l_orderkey`). Leaving the bare name in the hot
/// probe loop makes every lookup scan all row keys for a matching suffix.
pub(in crate::sql) fn bind_join_key_to_schema(
    expression: &ScalarExpr,
    schema: &[String],
) -> ScalarExpr {
    match expression {
        ScalarExpr::Column(name) => unique_physical_column(name, schema)
            .map_or_else(|| expression.clone(), ScalarExpr::Column),
        ScalarExpr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => {
            let expected = if key.is_empty() {
                format!("{qualifier}.{column}")
            } else {
                key.clone()
            };
            unique_physical_column(&expected, schema).map_or_else(
                || expression.clone(),
                |key| ScalarExpr::QualifiedColumn {
                    qualifier: qualifier.clone(),
                    column: column.clone(),
                    key,
                },
            )
        }
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(bind_join_key_to_schema(expr, schema)),
            ty: ty.clone(),
        },
        _ => expression.clone(),
    }
}

fn unique_physical_column(name: &str, schema: &[String]) -> Option<String> {
    if schema.iter().any(|column| column == name) {
        return Some(name.to_string());
    }
    let mut matches = schema.iter().filter(|key| {
        key.rsplit_once('.')
            .is_some_and(|(_, column)| column == name)
    });
    let first = matches.next()?;
    matches.next().is_none().then(|| first.clone())
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

pub(in crate::sql) fn join_schema_sample(columns: &[String]) -> ResultRow {
    columns
        .iter()
        .map(|column| (column.clone(), Value::Int(1)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_key_binding_uses_unique_exact_physical_column() {
        let schema = vec!["orders.o_orderkey".into(), "orders.o_custkey".into()];
        assert_eq!(
            bind_join_key_to_schema(&ScalarExpr::Column("o_orderkey".into()), &schema),
            ScalarExpr::Column("orders.o_orderkey".into())
        );
    }

    #[test]
    fn join_key_binding_keeps_ambiguous_bare_column() {
        let schema = vec!["left.id".into(), "right.id".into()];
        let expression = ScalarExpr::Column("id".into());
        assert_eq!(bind_join_key_to_schema(&expression, &schema), expression);
    }
}
