//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local table scans and recursive physical source assembly.

use super::{
    attach_qualifier_filter, bind_join_key_to_schema, build_info_schema_rows,
    build_table_function_row_stream, build_values_rows, combine_filters, decide_join_sides,
    execute_query_plan_output, execute_view_plan_output_with_parent_cache,
    has_filters_for_qualifier, is_score_provenance_column, join_conjuncts, join_schema_sample,
    null_row_for_schema, pad_nulls_for_from, physical_work_mem_bytes, propagated_join_filters,
    push_output_filter_into_query_plan, qualified_key, qualifier_filter, qualifier_for,
    qualify_source_operator, qualify_source_operator_with_columns,
    query_contains_volatile_function, query_cte_names, query_output_shared,
    table_function_empty_schema, ColumnPrune, CteScope, Engine, EngineExpressionEvaluator,
    EngineLateralSource, JoinExecutionStrategy, JoinKind, PlanSubqueryArena, QualifierFilters,
    QueryOutputMode, ResultRow, SQLError, SQLParam, ScalarExpr, ScopedEngineHook,
    ScoredDocumentSource, ScoredInput, SourcePlan, TableFunctionEvalContext, Value,
};

use uqa_planner::{AccessPathPlan, ComputePlan, RelationalPlan};

type StreamingLocalTableScan<'a> = (Box<dyn uqa_execution::PhysicalOperator + 'a>, bool);

pub(in crate::sql) struct EngineTableRowSource {
    table_name: String,
    table: std::sync::Arc<crate::TableState>,
    columns: Vec<String>,
    schema: Vec<String>,
    predicate: Option<uqa_execution::ProjectedPredicate>,
    estimated_cardinality: u64,
    after: Option<uqa_core::DocId>,
}

impl EngineTableRowSource {
    fn next_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> uqa_execution::ExecResult<Vec<uqa_execution::PhysicalRow>> {
        if max_rows == 0 {
            return Ok(Vec::new());
        }
        let store = self.table.document_store.read();
        let fields = self.columns.iter().map(String::as_str).collect::<Vec<_>>();
        let mut rows = Vec::with_capacity(max_rows);
        loop {
            // A source must not return an empty batch before end-of-stream:
            // TableScan treats it as EOF. Keep advancing storage pages when a
            // pushed predicate rejects an entire page, and fill the requested
            // output batch when selectivity permits it.
            let remaining = max_rows - rows.len();
            if remaining == 0 {
                break;
            }
            let direct_shared = store
                .next_shared_fields(self.after, remaining, &fields)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "scan shared projected fields from `{}`: {error}",
                        self.table_name
                    ))
                })?;
            if let Some(shared_rows) = direct_shared {
                let Some(last) = shared_rows.last().map(|(doc_id, _)| *doc_id) else {
                    break;
                };
                self.after = Some(last);
                for (_, shared) in shared_rows {
                    let keep = shared.with_projected(|projected| {
                        self.predicate
                            .as_ref()
                            .map_or(Ok(true), |predicate| predicate.keep(projected))
                    })?;
                    if keep {
                        let (values, projection) = shared.into_parts();
                        rows.push(uqa_execution::PhysicalRow::from_shared_values(
                            values, projection,
                        ));
                    }
                }
                continue;
            }
            let doc_ids = store.next_doc_ids(self.after, remaining).map_err(|error| {
                SQLError::Internal(format!(
                    "scan document ids for `{}`: {error}",
                    self.table_name
                ))
            })?;
            let Some(last) = doc_ids.last().copied() else {
                break;
            };
            self.after = Some(last);

            let shared_rows = store
                .get_shared_fields(&doc_ids, &fields)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "read shared projected fields from `{}`: {error}",
                        self.table_name
                    ))
                })?;
            if let Some(shared_rows) = shared_rows {
                if shared_rows.len() != doc_ids.len() {
                    return Err(SQLError::Internal(format!(
                        "table `{}` returned {} shared rows for {} document ids",
                        self.table_name,
                        shared_rows.len(),
                        doc_ids.len()
                    ))
                    .into());
                }
                let null = Value::Null;
                for shared in shared_rows {
                    let keep = if let Some(shared) = shared.as_ref() {
                        shared.with_projected(|projected| {
                            self.predicate
                                .as_ref()
                                .map_or(Ok(true), |predicate| predicate.keep(projected))
                        })?
                    } else {
                        let projected = vec![&null; fields.len()];
                        self.predicate
                            .as_ref()
                            .map_or(Ok(true), |predicate| predicate.keep(&projected))?
                    };
                    if !keep {
                        continue;
                    }
                    rows.push(match shared {
                        Some(shared) => {
                            let (values, projection) = shared.into_parts();
                            uqa_execution::PhysicalRow::from_shared_values(values, projection)
                        }
                        None => uqa_execution::PhysicalRow::nulls(fields.len()),
                    });
                }
            } else {
                let mut visited = 0usize;
                let mut predicate_error = None;
                store
                    .for_each_fields_multi_ref(&doc_ids, &fields, &mut |_, values| {
                        visited += 1;
                        if let Some(predicate) = self.predicate.as_ref() {
                            match predicate.keep(values) {
                                Ok(true) => {}
                                Ok(false) => return true,
                                Err(error) => {
                                    predicate_error = Some(error);
                                    return false;
                                }
                            }
                        }
                        rows.push(uqa_execution::PhysicalRow::from_values(
                            values.iter().map(|value| (*value).clone()).collect(),
                        ));
                        true
                    })
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "read projected fields from `{}`: {error}",
                            self.table_name
                        ))
                    })?;
                if let Some(error) = predicate_error {
                    return Err(error.into());
                }
                if visited != doc_ids.len() {
                    return Err(SQLError::Internal(format!(
                        "table `{}` visited {visited} of {} projected cursor rows",
                        self.table_name,
                        doc_ids.len()
                    ))
                    .into());
                }
            }
        }
        Ok(rows)
    }
}

impl uqa_execution::RowSource for EngineTableRowSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        Some(self.estimated_cardinality)
    }

    fn next_row(&mut self) -> uqa_execution::ExecResult<Option<ResultRow>> {
        Ok(self.next_batch(1)?.pop())
    }

    fn next_batch(&mut self, max_rows: usize) -> uqa_execution::ExecResult<Vec<ResultRow>> {
        let rows = self.next_physical_rows_batch(max_rows)?;
        let schema = uqa_execution::RowSchema::new(self.schema.clone());
        Ok(rows
            .iter()
            .map(|row| schema.view(row).to_result_row())
            .collect())
    }

    fn next_physical_batch(
        &mut self,
        max_rows: usize,
    ) -> uqa_execution::ExecResult<Vec<uqa_execution::PhysicalRow>> {
        self.next_physical_rows_batch(max_rows)
    }
}

pub(in crate::sql) fn try_streaming_local_table_scan<'a>(
    engine: &Engine,
    source: &SourcePlan,
    ctes: &CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    params: &[SQLParam],
) -> Result<Option<StreamingLocalTableScan<'a>>, SQLError> {
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
        return Ok(Some((
            Box::new(uqa_execution::ColumnSelection::with_mapping(scan, mapping)),
            false,
        )));
    }
    if engine.view_plan(name)?.is_some()
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
    let table_columns = engine
        .try_table_columns(name)
        .map_err(|error| SQLError::Internal(format!("read table columns for `{name}`: {error}")))?;
    // An unqualified reference is conservatively requested from every FROM
    // source during pruning.  The scan schema must still describe only real
    // table columns: advertising those over-inclusive requests as columns can
    // make later joins bind an unqualified name to a non-existent value.
    let columns = match wanted.as_ref() {
        Some(wanted) => table_columns
            .into_iter()
            .filter(|column| wanted.contains(column))
            .collect(),
        None => table_columns,
    };
    let schema = columns
        .iter()
        .map(|column| qualified_key(&qualifier, column))
        .collect();
    let predicate = qualifier_filter(filters, &qualifier)
        .map(|predicate| uqa_execution::ProjectedPredicate::compile(&predicate, &columns, params))
        .transpose()?
        .flatten();
    let filter_pushed = predicate.is_some();
    let source = EngineTableRowSource {
        table_name: name.clone(),
        table,
        columns,
        schema,
        predicate,
        estimated_cardinality: engine.table_doc_count(name)?,
        after: None,
    };
    Ok(Some((
        Box::new(uqa_execution::TableScan::new(Box::new(source))),
        filter_pushed,
    )))
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

            let Some((operator, filter_pushed)) =
                try_streaming_local_table_scan(engine, from, ctes, prune, filters, params)?
            else {
                return Err(SQLError::Unsupported(format!(
                    "relation `{name}` does not exist"
                )));
            };
            if filter_pushed {
                Ok(operator)
            } else {
                Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ))
            }
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
            let implicit_lateral_function = match right.as_ref() {
                SourcePlan::Function { name, .. } => {
                    let identity = name.to_ascii_lowercase();
                    let lower = crate::sql::builtin_function_dispatch_name(&identity);
                    !crate::operator_tree_bridge::is_operator_join_table_function(&lower)
                }
                _ => false,
            };
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
                            left_keys
                                .push(bind_join_key_to_schema(left_key, left_operator.schema()));
                            right_keys
                                .push(bind_join_key_to_schema(right_key, right_operator.schema()));
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
                ) => Ok(Box::new(
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
                )),
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
            if let Some(operator) =
                try_build_streaming_subquery_operator(engine, body, params, ctes)?
            {
                let source_columns = operator.schema().to_vec();
                let qualifier = alias.as_deref().unwrap_or_default();
                let operator = qualify_source_operator_with_columns(
                    operator,
                    &source_columns,
                    qualifier,
                    prune,
                    column_aliases,
                );
                return Ok(attach_qualifier_filter(
                    operator, qualifier, filters, engine, params, ctes,
                ));
            }
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

/// Build a single-consumer derived-table projection as a pull pipeline. These
/// query blocks have no relational feature that requires repeatability, so a
/// `SharedSpill` boundary would only serialize and read the same physical rows
/// once before the parent operator consumes them.
fn try_build_streaming_subquery_operator<'a>(
    engine: &'a Engine,
    body: &uqa_planner::QueryPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
) -> Result<Option<Box<dyn uqa_execution::PhysicalOperator + 'a>>, SQLError> {
    if !body.ctes.is_empty() || query_contains_volatile_function(engine, body)? {
        return Ok(None);
    }
    let RelationalPlan::QueryBlock(block) = &body.root else {
        return Ok(None);
    };
    if !matches!(block.compute, ComputePlan::Project)
        || !matches!(block.access, AccessPathPlan::Row)
        || block.from.is_none()
        || !block.subqueries.is_empty()
        || !block.order_by.is_empty()
        || block.limit.is_some()
        || block.offset.is_some()
        || block.distinct
        || !block.distinct_on.is_empty()
        || block
            .projections
            .iter()
            .any(|projection| matches!(projection.expr, ScalarExpr::Star))
    {
        return Ok(None);
    }

    let from = block
        .from
        .as_ref()
        .expect("derived-table FROM checked above");
    let column_prune = crate::sql::select::column_prune_for_stmt(engine, block, from);
    let qualifier_filters = crate::sql::select::qualifier_filters_for_stmt(engine, block, from);
    let mut operator = build_join_operator_with_ctes(
        engine,
        from,
        params,
        ctes,
        column_prune.as_ref(),
        qualifier_filters.as_ref(),
    )?;
    let residual = crate::sql::select::final_filter_after_qualifier_pushdown(
        engine,
        block,
        from,
        qualifier_filters.as_ref(),
    );
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    if let Some(predicate) = residual {
        operator = Box::new(uqa_execution::Filter::with_evaluator(
            operator,
            predicate,
            std::sync::Arc::clone(&evaluator),
        ));
    }
    let mut projections = crate::sql::select::physical_projections(&block.projections);
    crate::sql::select::append_score_provenance_projections(&mut projections, operator.schema());
    Ok(Some(Box::new(uqa_execution::Project::with_evaluator(
        operator,
        projections,
        evaluator,
    ))))
}
