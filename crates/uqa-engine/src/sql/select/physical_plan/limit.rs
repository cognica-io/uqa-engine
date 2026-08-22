//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered slicing and `FETCH ... WITH TIES` physical operators.

use std::sync::Arc;

use super::{
    physical_work_mem_bytes, resolve_fetch_limit_with_ties, resolve_limit_offset_with_ctes,
    resolve_order_expression, CteScope, Engine, OutputColumnMapping, QueryBlockPlan, SQLError,
    SQLParam, ScalarExpr, SharedExpressionEvaluator, ORDER_SET_COLUMN_PREFIX,
};

#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn attach_order_limit<'a>(
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
    recheck_source: Option<crate::sql::select::LockRowsRecheckSource>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::{ExternalSort, Limit};

    // DISTINCT executes before slicing. Its internal execution statement keeps the semantic flag so ORDER BY values are projected exactly once, but clears the count; only a statement that still has that count attaches the tie boundary here.
    let with_ties = statement.with_ties && statement.limit.is_some();
    let offset =
        resolve_limit_offset_with_ctes(statement.offset.as_ref(), engine, params, "OFFSET", ctes)?;
    let limit = if with_ties {
        Some(resolve_fetch_limit_with_ties(
            statement.limit.as_ref(),
            engine,
            params,
            ctes,
        )?)
    } else {
        resolve_limit_offset_with_ctes(statement.limit.as_ref(), engine, params, "LIMIT", ctes)?
    };
    let mut tie_keys = None;
    if !statement.order_by.is_empty() {
        let work_mem_bytes = physical_work_mem_bytes(engine)?;
        let keys = resolved_sort_keys(statement, output_columns, None)?;
        if with_ties {
            tie_keys = Some(keys.clone());
        }
        let keep = if let Some(limit) = limit {
            let keep = offset
                .unwrap_or(0)
                .checked_add(limit)
                .ok_or_else(|| SQLError::TypeMismatch("OFFSET + LIMIT overflow".into()))?;
            Some(usize::try_from(keep).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "OFFSET + LIMIT {keep} exceeds the platform row-count range"
                ))
            })?)
        } else {
            None
        };
        let required_ordering = keys
            .iter()
            .map(|key| {
                uqa_execution::order_expression_position(operator.row_schema(), &key.expr).map(
                    |position| uqa_execution::PhysicalOrder {
                        position,
                        descending: key.descending,
                        nulls_first: Some(key.nulls_first.unwrap_or(key.descending)),
                        nullable: true,
                    },
                )
            })
            .collect::<Option<Vec<_>>>();
        let already_ordered = required_ordering.as_ref().is_some_and(|required| {
            uqa_execution::ordering_satisfies(operator.output_ordering(), required)
        });
        if !already_ordered {
            // A locking query must keep the complete sorted candidate stream: SKIP LOCKED skips rows, and a tuple-local recheck can drop a changed candidate, in which case PostgreSQL 18 surfaces the next candidate in sort order instead of returning fewer rows.
            operator = Box::new(ExternalSort::new(
                operator,
                keys,
                Arc::clone(&evaluator),
                keep.filter(|_| statement.locking.is_empty() && !with_ties),
                work_mem_bytes,
            ));
        }
    }
    if !statement.locking.is_empty() {
        let max_rows = if with_ties {
            None
        } else {
            limit
                .map(|limit| {
                    offset
                        .unwrap_or(0)
                        .checked_add(limit)
                        .ok_or_else(|| SQLError::TypeMismatch("OFFSET + LIMIT overflow".into()))
                })
                .transpose()?
        };
        operator = crate::sql::select::attach_lock_rows(
            engine,
            operator,
            statement,
            params,
            ctes,
            max_rows,
            recheck_source,
        )?;
    }
    if with_ties {
        operator = Box::new(Limit::with_ties(
            operator,
            offset.unwrap_or(0),
            limit.expect("WITH TIES row count resolved above"),
            tie_keys.ok_or_else(|| {
                SQLError::Internal("FETCH ... WITH TIES has no ORDER BY keys".into())
            })?,
            evaluator,
        ));
    } else if offset.is_some() || limit.is_some() {
        operator = Box::new(Limit::new(operator, offset.unwrap_or(0), limit));
    }
    Ok(operator)
}

pub(super) fn resolved_sort_keys(
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    hidden_schema: Option<&uqa_execution::RowSchema>,
) -> Result<Vec<uqa_execution::SortKey>, SQLError> {
    statement.order_by.iter().enumerate().try_fold(
        Vec::<uqa_execution::SortKey>::new(),
        |mut keys, (index, order)| {
            let hidden = format!("{ORDER_SET_COLUMN_PREFIX}{index}");
            let expr = if hidden_schema.is_some_and(|schema| schema.position(&hidden).is_some()) {
                ScalarExpr::Column(hidden)
            } else {
                resolve_order_expression(&order.expr, output_columns)?
            };
            let key = uqa_execution::SortKey {
                expr,
                descending: order.descending,
                nulls_first: order
                    .nulls
                    .map(|nulls| matches!(nulls, uqa_sql::ast::NullsOrder::First)),
            };
            if !keys
                .iter()
                .any(|existing| crate::sql::aggregates::exprs_match(&existing.expr, &key.expr))
            {
                keys.push(key);
            }
            Ok(keys)
        },
    )
}
