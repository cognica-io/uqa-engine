//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered slicing and `FETCH ... WITH TIES` physical operators.

use super::ordering::resolve_order_expression;
use super::row_count::{resolve_fetch_limit_with_ties, resolve_limit_offset_with_ctes};
use super::RelationalContext;
use crate::query::projection::physical_work_mem_bytes;
use crate::query::runtime::QueryRuntimeView;
use crate::query::{CteScope, OutputColumnMapping};
use crate::SharedExpressionEvaluator;
use std::sync::Arc;
use uqa_sql::{plan::QueryBlockPlan, SQLError, SQLParam};

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SELECT scope inputs aligned"
)]
pub fn attach_order_limit<'a, S: Clone + 'static>(
    mut operator: Box<dyn crate::PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    context: RelationalContext<'a, S>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    runtime: QueryRuntimeView<'a>,
    evaluator: SharedExpressionEvaluator<'a>,
    recheck_source: Option<crate::query::recheck_source::LockRowsRecheckSource<S>>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    use crate::{ExternalSort, Limit};

    // DISTINCT executes before slicing. Its internal execution statement keeps the semantic flag so ORDER BY values are projected exactly once, but clears the count; only a statement that still has that count attaches the tie boundary here.
    let with_ties = statement.with_ties && statement.limit.is_some();
    let offset =
        resolve_limit_offset_with_ctes(statement.offset.as_ref(), context, params, "OFFSET", ctes)?;
    let limit = if with_ties {
        Some(resolve_fetch_limit_with_ties(
            statement.limit.as_ref(),
            context,
            params,
            ctes,
        )?)
    } else {
        resolve_limit_offset_with_ctes(statement.limit.as_ref(), context, params, "LIMIT", ctes)?
    };
    let mut tie_keys = None;
    if !statement.order_by.is_empty() {
        let work_mem_bytes = physical_work_mem_bytes(runtime)?;
        let keys = resolved_sort_keys(statement, output_columns, Some(operator.row_schema()))?;
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
                crate::order_expression_position(operator.row_schema(), &key.expr).map(|position| {
                    crate::PhysicalOrder {
                        position,
                        descending: key.descending,
                        nulls_first: Some(key.nulls_first.unwrap_or(key.descending)),
                        nullable: true,
                    }
                })
            })
            .collect::<Option<Vec<_>>>();
        let already_ordered = required_ordering.as_ref().is_some_and(|required| {
            crate::ordering_satisfies(operator.output_ordering(), required)
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
            if ctes.scans_backwards() {
                operator = crate::prepare_backward_scan(operator);
            }
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
        operator = super::attach_lock_rows(
            context,
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

pub fn resolved_sort_keys(
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    hidden_schema: Option<&crate::RowSchema>,
) -> Result<Vec<crate::SortKey>, SQLError> {
    statement
        .order_by
        .iter()
        .try_fold(Vec::<crate::SortKey>::new(), |mut keys, order| {
            let expr = resolve_order_expression(&order.expr, output_columns)?;
            if let Some(schema) = hidden_schema {
                if let Some(ty) = crate::scalar_type(&expr, schema, &[])? {
                    crate::require_ordering_operator(&ty)?;
                }
            }
            let key = crate::SortKey {
                expr,
                descending: order.descending,
                nulls_first: order
                    .nulls
                    .map(|nulls| matches!(nulls, uqa_sql::ast::NullsOrder::First)),
            };
            if !keys.iter().any(|existing| {
                uqa_sql::semantics::aggregates::exprs_match(&existing.expr, &key.expr)
            }) {
                keys.push(key);
            }
            Ok(keys)
        })
}
