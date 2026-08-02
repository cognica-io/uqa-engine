//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local table scans and recursive physical source assembly.

use super::{
    attach_qualifier_filter, build_info_schema_rows, build_table_function_row_stream,
    build_values_rows, combine_filters, decide_join_sides, execute_query_plan_output,
    execute_view_plan_output_with_parent_cache, has_filters_for_qualifier,
    is_score_provenance_column, join_conjuncts, join_schema_sample, null_row_for_schema,
    pad_nulls_for_from, physical_work_mem_bytes, propagated_join_filters,
    push_output_filter_into_query_plan, qualified_key, qualifier_filter, qualifier_for,
    qualify_source_operator, qualify_source_operator_with_columns,
    query_contains_volatile_function, query_cte_names, query_output_shared,
    table_function_empty_schema, BTreeSet, ColumnPrune, CteScope, Engine,
    EngineExpressionEvaluator, EngineLateralSource, JoinExecutionStrategy, JoinKind,
    PlanSubqueryArena, QualifierFilters, QueryOutputMode, ResultRow, SQLError, SQLParam,
    ScalarExpr, ScopedEngineHook, ScoredDocumentSource, ScoredInput, SourcePlan,
    TableFunctionEvalContext,
};

pub(in crate::sql) struct EngineTableRowSource {
    table_name: String,
    table: std::sync::Arc<crate::TableState>,
    qualifier: String,
    wanted: Option<BTreeSet<String>>,
    schema: Vec<String>,
    after: Option<uqa_core::DocId>,
}

impl uqa_execution::RowSource for EngineTableRowSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn next_row(&mut self) -> uqa_execution::ExecResult<Option<ResultRow>> {
        let store = self.table.document_store.read();
        let Some(doc_id) = store.next_doc_id(self.after).map_err(|error| {
            SQLError::Internal(format!(
                "scan document ids for `{}`: {error}",
                self.table_name
            ))
        })?
        else {
            return Ok(None);
        };
        self.after = Some(doc_id);
        let document = store.get(doc_id).map_err(|error| {
            SQLError::Internal(format!(
                "read `{}` document {doc_id}: {error}",
                self.table_name
            ))
        })?;
        let document = document.ok_or_else(|| {
            SQLError::Internal(format!(
                "table `{}` cursor returned document {doc_id}, but materialization omitted it",
                self.table_name
            ))
        })?;
        let mut row = ResultRow::new();
        for (column, value) in document {
            if self
                .wanted
                .as_ref()
                .is_some_and(|wanted| !wanted.contains(&column))
            {
                continue;
            }
            row.insert(qualified_key(&self.qualifier, &column), value);
        }
        Ok(Some(row))
    }
}

pub(in crate::sql) fn try_streaming_local_table_scan<'a>(
    engine: &Engine,
    source: &SourcePlan,
    ctes: &CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Option<Box<dyn uqa_execution::PhysicalOperator + 'a>>, SQLError> {
    let SourcePlan::Table { name, alias } = source else {
        return Ok(None);
    };
    let qualifier = qualifier_for(name, alias.as_deref());
    if let Some(materialized) = ctes.rows.get(name).cloned() {
        if has_filters_for_qualifier(filters, &qualifier) {
            return Ok(None);
        }
        let mapping = materialized
            .schema()
            .iter()
            .filter_map(|source| {
                let column = if is_score_provenance_column(source) {
                    source.as_str()
                } else {
                    source
                        .rsplit_once('.')
                        .map_or(source.as_str(), |(_, column)| column)
                };
                if !is_score_provenance_column(source)
                    && prune
                        .and_then(|prune| prune.get(&qualifier))
                        .is_some_and(|wanted| !wanted.contains(column))
                {
                    return None;
                }
                Some((qualified_key(&qualifier, column), source.clone()))
            })
            .collect();
        let scan: Box<dyn uqa_execution::PhysicalOperator + 'a> =
            Box::new(uqa_execution::SharedSpillScan::new(materialized));
        return Ok(Some(Box::new(
            uqa_execution::ColumnSelection::with_mapping(scan, mapping),
        )));
    }
    if has_filters_for_qualifier(filters, &qualifier)
        || engine.view_plan(name)?.is_some()
        || engine
            .foreign_table(name)
            .map_err(SQLError::Unsupported)?
            .is_some()
    {
        return Ok(None);
    }
    let Some(table) = engine
        .try_table(name)
        .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?
    else {
        return Ok(None);
    };
    let wanted = prune.and_then(|prune| prune.get(&qualifier)).cloned();
    let columns = if let Some(wanted) = wanted.as_ref() {
        wanted.iter().cloned().collect()
    } else {
        engine.try_table_columns(name).map_err(|error| {
            SQLError::Internal(format!("read table columns for `{name}`: {error}"))
        })?
    };
    let schema = columns
        .into_iter()
        .map(|column| qualified_key(&qualifier, &column))
        .collect();
    let source = EngineTableRowSource {
        table_name: name.clone(),
        table,
        qualifier,
        wanted,
        schema,
        after: None,
    };
    Ok(Some(Box::new(uqa_execution::TableScan::new(Box::new(
        source,
    )))))
}

/// Build a complete FROM source as a pull-based physical operator. Unlike the
/// compatibility `build_join_rows_*` entry points below, this is the query
/// executor's primary path and never collects a join, view, CTE, derived table,
/// or table-function result into a cardinality-sized `Vec`.
pub(in crate::sql) fn build_join_operator_with_ctes<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::{HashJoin, LateralJoin, NestedLoopJoin, PhysicalOperator};

    match from {
        SourcePlan::Table { name, alias } => {
            let qualifier = qualifier_for(name, alias.as_deref());
            if let Some(materialized) = ctes.rows.get(name).cloned() {
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(materialized));
                let operator = qualify_source_operator(scan, &qualifier, prune);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(plan) = engine.view_plan(name)? {
                let specialized_plan = filters
                    .and_then(|filters| filters.get(&qualifier))
                    .filter(|filters| !filters.is_empty())
                    .and_then(|filters| combine_filters(filters.iter().cloned()))
                    .map(|filter| {
                        push_output_filter_into_query_plan(engine, &plan, &qualifier, &filter, None)
                    })
                    .transpose()?
                    .flatten();
                let execution_plan = specialized_plan.as_ref().unwrap_or(&plan);
                let local_cte_names = query_cte_names(execution_plan);
                let is_volatile = query_contains_volatile_function(engine, execution_plan)?;
                let output = if is_volatile {
                    let mut scoped = ctes.clone();
                    execute_query_plan_output(
                        engine,
                        execution_plan,
                        params,
                        &mut scoped,
                        QueryOutputMode::SharedSpill,
                    )?
                } else {
                    execute_view_plan_output_with_parent_cache(
                        engine,
                        execution_plan,
                        params,
                        ctes,
                        &local_cte_names,
                    )?
                };
                let columns = output.internal_columns.clone();
                let shared = query_output_shared(output, "view")?;
                if !is_volatile && specialized_plan.is_none() {
                    ctes.insert_shared(name.clone(), shared.clone());
                }
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(shared));
                let operator =
                    qualify_source_operator_with_columns(scan, &columns, &qualifier, prune, &[]);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(rows) = build_info_schema_rows(engine, name)? {
                let columns: Vec<String> = rows
                    .first()
                    .map(|row| row.keys().cloned().collect())
                    .unwrap_or_default();
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::TableScan::from_rows(columns.clone(), rows));
                let operator =
                    qualify_source_operator_with_columns(scan, &columns, &qualifier, prune, &[]);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if engine
                .foreign_table(name)
                .map_err(SQLError::Unsupported)?
                .is_some()
            {
                let rows = engine
                    .scan_foreign_table_stream(name, None, &[], None)
                    .map_err(SQLError::Unsupported)?;
                let columns = engine
                    .foreign_table_columns(name)
                    .map_err(SQLError::Unsupported)?;
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::RowIteratorScan::new(
                        columns.clone(),
                        Box::new(rows.map(|row| {
                            row.map_err(SQLError::Unsupported)
                                .map_err(uqa_execution::ExecError::from)
                        })),
                    ));
                let operator =
                    qualify_source_operator_with_columns(scan, &columns, &qualifier, prune, &[]);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(predicate) = qualifier_filter(filters, &qualifier)
                .filter(uqa_planner::optimizer::contains_retrieval)
            {
                let entries = crate::operator_tree_bridge::run_optimised(
                    engine,
                    name,
                    Some(&predicate),
                    params,
                )?
                .ok_or_else(|| {
                    SQLError::Unsupported(format!(
                        "JOIN filter retrieval predicate for `{qualifier}` cannot be represented by the shared operator IR"
                    ))
                })?;
                let table = engine.require_table(name)?;
                let columns = engine.try_table_columns(name).map_err(|error| {
                    SQLError::Internal(format!("read table columns for `{name}`: {error}"))
                })?;
                let source = ScoredDocumentSource::new(
                    name,
                    table,
                    ScoredInput::entries(entries, true),
                    columns,
                    None,
                    None,
                );
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::TableScan::new(Box::new(source)));
                return Ok(qualify_source_operator(scan, &qualifier, prune));
            }

            let Some(operator) = try_streaming_local_table_scan(engine, from, ctes, prune, None)?
            else {
                return Err(SQLError::Unsupported(format!(
                    "relation `{name}` does not exist"
                )));
            };
            Ok(attach_qualifier_filter(
                operator, &qualifier, filters, engine, params, ctes,
            ))
        }
        SourcePlan::Join {
            left,
            right,
            kind,
            on,
            lateral,
            strategy,
        } => {
            let left_filters = filters
                .and_then(|filters| propagated_join_filters(filters, right, left, on.as_ref()));
            let left_filter_ref = left_filters.as_ref().or(filters);
            let left_operator =
                build_join_operator_with_ctes(engine, left, params, ctes, prune, left_filter_ref)?;
            let implicit_lateral_function = matches!(right.as_ref(), SourcePlan::Function { .. });
            if *lateral || implicit_lateral_function {
                if !matches!(strategy, JoinExecutionStrategy::Auto) {
                    return Err(SQLError::Internal(
                        "optimizer selected a hash strategy for a lateral join".into(),
                    ));
                }
                let left_nulls = null_row_for_schema(left_operator.schema());
                let mut right_nulls = ResultRow::new();
                pad_nulls_for_from(&mut right_nulls, right, engine)?;
                let source = EngineLateralSource {
                    engine,
                    right: (**right).clone(),
                    on: on.clone(),
                    params,
                    ctes: ctes.clone(),
                };
                return Ok(Box::new(LateralJoin::new(
                    left_operator,
                    Box::new(source),
                    *kind,
                    left_nulls,
                    right_nulls,
                )));
            }

            let right_filters = filters
                .and_then(|filters| propagated_join_filters(filters, left, right, on.as_ref()));
            let right_filter_ref = right_filters.as_ref().or(filters);
            let right_operator = build_join_operator_with_ctes(
                engine,
                right,
                params,
                ctes,
                prune,
                right_filter_ref,
            )?;

            let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
            let hash_plan = if matches!(kind, JoinKind::Cross) {
                None
            } else {
                on.as_ref().and_then(|predicate| {
                    let conjuncts = join_conjuncts(predicate);
                    let scoped_hook = ScopedEngineHook::new(engine, ctes);
                    let subquery_arena =
                        PlanSubqueryArena::new(&ctes.scalar_subqueries, Some(&scoped_hook));
                    let left_sample = join_schema_sample(left_operator.schema());
                    let right_sample = join_schema_sample(right_operator.schema());
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
                        if let Some((left_key, right_key)) = decide_join_sides(
                            &scoped_hook,
                            &subquery_arena,
                            std::slice::from_ref(&left_sample),
                            std::slice::from_ref(&right_sample),
                            lhs,
                            rhs,
                            params,
                        ) {
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
            let work_mem = physical_work_mem_bytes(engine)?;
            match (strategy, hash_plan) {
                (
                    JoinExecutionStrategy::Auto | JoinExecutionStrategy::Hash,
                    Some((left_keys, right_keys, residual)),
                ) => Ok(Box::new(HashJoin::new_with_work_mem_and_predicate(
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
                ))),
                (JoinExecutionStrategy::Auto, None) => {
                    Ok(Box::new(NestedLoopJoin::new_with_work_mem(
                        left_operator,
                        right_operator,
                        *kind,
                        on.clone(),
                        evaluator,
                        left_nulls,
                        right_nulls,
                        work_mem,
                    )))
                }
                (JoinExecutionStrategy::Hash, None) => Err(SQLError::Internal(
                    "DPccp hash-join strategy has no splittable equality predicate".into(),
                )),
            }
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let hook = ScopedEngineHook::new(engine, ctes);
            let rows = build_values_rows(
                rows,
                alias.as_deref(),
                column_aliases,
                params,
                &hook,
                &hook,
                &ctes.scalar_subqueries,
            )?;
            let columns = rows.first().map_or_else(
                || {
                    column_aliases
                        .iter()
                        .map(|column| {
                            alias
                                .as_deref()
                                .map_or_else(|| column.clone(), |qual| qualified_key(qual, column))
                        })
                        .collect()
                },
                |row| row.keys().cloned().collect(),
            );
            Ok(Box::new(uqa_execution::TableScan::from_rows(columns, rows)))
        }
        SourcePlan::Function {
            name,
            args,
            alias,
            column_aliases,
            column_types,
        } => {
            let hook = ScopedEngineHook::new(engine, ctes);
            let context = TableFunctionEvalContext::new(
                engine,
                params,
                &hook,
                &hook,
                &ctes.scalar_subqueries,
            );
            let mut rows = build_table_function_row_stream(
                &context,
                name,
                args,
                alias.as_deref(),
                column_aliases,
                column_types,
            )?;
            let first = rows
                .next()
                .transpose()
                .map_err(crate::sql::select::physical_exec_error)?;
            let columns = first.as_ref().map_or_else(
                || table_function_empty_schema(name, alias.as_deref(), column_aliases),
                |row| row.keys().cloned().collect(),
            );
            let rows = first.into_iter().map(Ok).chain(rows);
            Ok(Box::new(uqa_execution::RowIteratorScan::new(
                columns,
                Box::new(rows),
            )))
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let local_cte_names = query_cte_names(body);
            let output = execute_view_plan_output_with_parent_cache(
                engine,
                body,
                params,
                ctes,
                &local_cte_names,
            )?;
            let source_columns = output.internal_columns.clone();
            let operator = output.into_operator();
            let qualifier = alias.as_deref().unwrap_or_default();
            let operator = qualify_source_operator_with_columns(
                operator,
                &source_columns,
                qualifier,
                prune,
                column_aliases,
            );
            Ok(attach_qualifier_filter(
                operator, qualifier, filters, engine, params, ctes,
            ))
        }
    }
}
