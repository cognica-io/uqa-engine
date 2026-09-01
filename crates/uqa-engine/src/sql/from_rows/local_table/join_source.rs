//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for lateral, hash, and nested-loop joins.

use super::{
    alias_join_operator, bind_source_plan_schema, build_join_operator_with_ctes_at_path,
    decide_join_sides, join_conjuncts, join_using_predicate, null_row_for_schema,
    physical_work_mem_bytes, propagated_join_filters, resolve_join_using, shape_join_using_output,
    validate_join_on_schema, ColumnPrune, CteScope, Engine, EngineExpressionEvaluator,
    EngineLateralSource, JoinExecutionStrategy, JoinKind, QualifierFilters, SQLError, SQLParam,
    ScalarExpr, SourcePlan,
};
use uqa_execution::{HashJoin, LateralJoin, NestedLoopJoin, PhysicalOperator};

/// Build the physical operator for a join source.
#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(super) fn build_join_source_operator<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    recheck_path: Option<Vec<u8>>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Join {
            left,
            right,
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            lateral,
            strategy,
        } => {
            let left_recheck_path = recheck_path.as_ref().map(|path| {
                let mut path = path.clone();
                path.push(0);
                path
            });
            let right_recheck_path = recheck_path.as_ref().map(|path| {
                let mut path = path.clone();
                path.push(1);
                path
            });
            let left_filters = filters
                .and_then(|filters| propagated_join_filters(filters, right, left, on.as_ref()));
            let left_filter_ref = left_filters.as_ref().or(filters);
            let left_operator = build_join_operator_with_ctes_at_path(
                engine,
                left,
                params,
                ctes,
                prune,
                left_filter_ref,
                left_recheck_path,
            )?;
            let implicit_lateral_function = match right.as_ref() {
                SourcePlan::Function { name, .. } => {
                    let identity = name.to_ascii_lowercase();
                    let lower = crate::sql::builtin_function_dispatch_name(&identity);
                    !crate::operator_tree_bridge::is_operator_join_table_function(&lower)
                }
                SourcePlan::FunctionGroup { .. } => true,
                _ => false,
            };
            if *lateral || implicit_lateral_function {
                if !matches!(strategy, JoinExecutionStrategy::Auto) {
                    return Err(SQLError::Internal(
                        "optimizer selected a hash strategy for a lateral join".into(),
                    ));
                }
                let left_schema = left_operator.row_schema().clone();
                let left_nulls = null_row_for_schema(left_schema.columns());
                let right_schema =
                    bind_source_plan_schema(engine, right, params, ctes, Some(&left_schema))?;
                validate_join_on_schema(
                    engine,
                    on.as_ref(),
                    &left_schema,
                    &right_schema,
                    params,
                    ctes,
                )?;
                let right_nulls = null_row_for_schema(right_schema.columns());
                let resolved_using =
                    resolve_join_using(using.as_ref(), *natural, &left_schema, &right_schema)?;
                let effective_on = resolved_using
                    .as_ref()
                    .and_then(|using| join_using_predicate(using, &left_schema, &right_schema))
                    .or_else(|| on.clone());
                let pinned_right = right_recheck_path
                    .as_deref()
                    .and_then(|path| ctes.recheck_source_row(path))
                    .map(|source| {
                        source
                            .schema
                            .relayout_physical_row(source.row, &right_schema)
                            .map(|row| {
                                uqa_execution::OwnedPhysicalRow::new(right_schema.clone(), row)
                            })
                            .map_err(crate::sql::select::physical_exec_error)
                    })
                    .transpose()?;
                let source = EngineLateralSource {
                    engine,
                    right: (**right).clone(),
                    on: effective_on,
                    params,
                    ctes: ctes.clone(),
                    right_schema: right_schema.clone(),
                    pinned_right,
                };
                let joined: Box<dyn PhysicalOperator + 'a> =
                    Box::new(LateralJoin::new_with_right_schema(
                        left_operator,
                        Box::new(source),
                        *kind,
                        left_nulls,
                        right_nulls,
                        right_schema.clone(),
                    ));
                let joined = if let Some(using) = resolved_using.as_ref() {
                    shape_join_using_output(joined, *kind, &left_schema, &right_schema, using)
                } else {
                    Ok(joined)
                }?;
                return alias_join_operator(joined, alias.as_deref(), column_aliases);
            }

            let right_filters = filters
                .and_then(|filters| propagated_join_filters(filters, left, right, on.as_ref()));
            let right_filter_ref = right_filters.as_ref().or(filters);
            let right_operator = build_join_operator_with_ctes_at_path(
                engine,
                right,
                params,
                ctes,
                prune,
                right_filter_ref,
                right_recheck_path,
            )?;

            let left_schema = left_operator.row_schema().clone();
            let right_schema = right_operator.row_schema().clone();
            validate_join_on_schema(
                engine,
                on.as_ref(),
                &left_schema,
                &right_schema,
                params,
                ctes,
            )?;
            let resolved_using =
                resolve_join_using(using.as_ref(), *natural, &left_schema, &right_schema)?;
            let effective_on = resolved_using
                .as_ref()
                .and_then(|using| join_using_predicate(using, &left_schema, &right_schema))
                .or_else(|| on.clone());

            let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
            let hash_plan = if matches!(kind, JoinKind::Cross) {
                None
            } else {
                effective_on.as_ref().and_then(|predicate| {
                    let conjuncts = join_conjuncts(predicate);
                    let mut left_keys = Vec::with_capacity(conjuncts.len());
                    let mut right_keys = Vec::with_capacity(conjuncts.len());
                    let mut residual = Vec::new();
                    for conjunct in conjuncts {
                        let ScalarExpr::Binary {
                            op: uqa_sql::ast::BinaryOp::Equal,
                            lhs,
                            rhs,
                        } = conjunct
                        else {
                            residual.push(conjunct.clone());
                            continue;
                        };
                        if let Some((left_key, right_key)) =
                            decide_join_sides(&left_schema, &right_schema, lhs, rhs)
                        {
                            left_keys.push(left_key.clone());
                            right_keys.push(right_key.clone());
                        } else {
                            residual.push(conjunct.clone());
                        }
                    }
                    if left_keys.is_empty() {
                        return None;
                    }
                    let residual = match residual.len() {
                        0 => None,
                        1 => residual.pop(),
                        _ => Some(ScalarExpr::And(residual)),
                    };
                    Some((left_keys, right_keys, residual))
                })
            };
            let left_nulls = null_row_for_schema(left_operator.schema());
            let right_nulls = null_row_for_schema(right_operator.schema());
            let work_mem = physical_work_mem_bytes(engine.query_runtime_view())?;
            let joined: Box<dyn PhysicalOperator + 'a> = match (strategy, hash_plan) {
                (
                    JoinExecutionStrategy::Auto | JoinExecutionStrategy::Hash,
                    Some((left_keys, right_keys, residual)),
                ) => Box::new(
                    HashJoin::try_new_with_work_mem_and_predicate(
                        left_operator,
                        right_operator,
                        *kind,
                        left_keys,
                        right_keys,
                        residual,
                        evaluator,
                        left_nulls,
                        right_nulls,
                        work_mem,
                        params,
                    )
                    .map_err(crate::sql::select::physical_exec_error)?,
                ),
                (JoinExecutionStrategy::Auto, None) => Box::new(NestedLoopJoin::new_with_work_mem(
                    left_operator,
                    right_operator,
                    *kind,
                    effective_on,
                    evaluator,
                    left_nulls,
                    right_nulls,
                    work_mem,
                )),
                (JoinExecutionStrategy::Hash, None) => {
                    return Err(SQLError::Internal(
                        "DPccp hash-join strategy has no splittable equality predicate".into(),
                    ));
                }
            };
            let joined = if let Some(using) = resolved_using.as_ref() {
                shape_join_using_output(joined, *kind, &left_schema, &right_schema, using)
            } else {
                Ok(joined)
            }?;
            alias_join_operator(joined, alias.as_deref(), column_aliases)
        }
        _ => unreachable!("join source builder called for a different source kind"),
    }
}
