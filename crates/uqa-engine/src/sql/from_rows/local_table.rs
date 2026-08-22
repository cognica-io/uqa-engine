//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local table scans and recursive physical source assembly.

use super::{
    apply_table_function_aliases, attach_qualifier_filter, build_info_schema_rows,
    build_table_function_row_stream, build_values_physical_rows, combine_filters,
    decide_join_sides, execute_query_plan_output, execute_view_plan_output_with_parent_cache,
    has_filters_for_qualifier, is_score_provenance_column, join_conjuncts, join_using_predicate,
    multi_unnest_internal_columns, null_row_for_schema, physical_work_mem_bytes,
    propagated_join_filters, push_output_filter_into_query_plan, qualifier_filter, qualifier_for,
    qualify_source_operator, qualify_source_operator_with_columns,
    query_contains_volatile_function, query_cte_names, query_output_shared, resolve_join_using,
    shape_join_using_output, table_function_column_types, table_function_empty_schema,
    validate_table_function_alias_count, ColumnPrune, CteScope, Engine, EngineExpressionEvaluator,
    EngineLateralSource, JoinExecutionStrategy, JoinKind, QualifierFilters, QueryOutputMode,
    ResultRow, SQLError, SQLParam, ScalarExpr, ScopedEngineHook, ScoredDocumentSource, ScoredInput,
    SourceEvalContext, SourcePlan, TableFunctionCall, TableFunctionTypeRequest, Value,
    TABLE_FUNCTION_ORDINALITY_COLUMN,
};

use crate::sql::select::{
    apply_propagated_view_lock, bind_source_plan_schema, materialize_plan_ctes, resolve_row_locks,
};
use crate::sql::virtual_relation_schema;
use std::sync::Arc;
use uqa_planner::{AccessPathPlan, ComputePlan, RelationalPlan};

#[path = "local_table_command_scan.rs"]
mod command_scan;

type StreamingLocalTableScan<'a> = (Box<dyn uqa_execution::PhysicalOperator + 'a>, bool);
type SharedLockOrigin = (Arc<str>, Arc<str>);

pub(in crate::sql) struct EngineTableRowSource {
    table_name: String,
    table: std::sync::Arc<crate::TableState>,
    column_definitions: Vec<uqa_sql::ast::ColumnDef>,
    columns: Vec<String>,
    schema: Vec<String>,
    physical_schema: uqa_execution::RowSchema,
    predicate: Option<uqa_execution::ProjectedPredicate>,
    estimated_cardinality: u64,
    after: Option<uqa_core::DocId>,
    lock_origin: Option<SharedLockOrigin>,
    recheck_pins: Option<Arc<Vec<crate::sql::select::RecheckDoc>>>,
    recheck_cursor: usize,
    command_changes: Option<
        Arc<
            std::collections::BTreeMap<
                uqa_core::DocId,
                Option<uqa_storage::document_store::Document>,
            >,
        >,
    >,
    command_change_after: Option<uqa_core::DocId>,
    command_base_after: Option<uqa_core::DocId>,
    command_base_ids: std::collections::VecDeque<uqa_core::DocId>,
    command_base_exhausted: bool,
}

fn table_lock_origin(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    enabled: bool,
) -> Result<Option<SharedLockOrigin>, SQLError> {
    if !enabled {
        return Ok(None);
    }
    let storage_name = engine
        .try_resolve_table_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        .unwrap_or_else(|| table.to_string());
    Ok(Some((
        Arc::<str>::from(qualifier),
        Arc::<str>::from(storage_name),
    )))
}

impl EngineTableRowSource {
    fn next_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> uqa_execution::ExecResult<Vec<uqa_execution::PhysicalRow>> {
        if max_rows == 0 {
            return Ok(Vec::new());
        }
        if self.recheck_pins.is_some() {
            return self.next_pinned_physical_rows_batch(max_rows);
        }
        if self.command_changes.is_some() {
            return self.next_command_physical_rows_batch(max_rows);
        }
        if crate::engine_generated::projection_contains_virtual_generated_column(
            &self.column_definitions,
            &self.columns,
        ) {
            return self.next_virtual_physical_rows_batch(max_rows);
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
                for (doc_id, shared) in shared_rows {
                    let keep = shared.with_projected(|projected| {
                        self.predicate
                            .as_ref()
                            .map_or(Ok(true), |predicate| predicate.keep(projected))
                    })?;
                    if keep {
                        let (values, projection) = shared.into_parts();
                        rows.push(self.with_lock_identity(
                            uqa_execution::PhysicalRow::from_shared_values(values, projection),
                            doc_id,
                        )?);
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
                for (doc_id, shared) in doc_ids.iter().copied().zip(shared_rows) {
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
                    rows.push(self.with_lock_identity(
                        match shared {
                            Some(shared) => {
                                let (values, projection) = shared.into_parts();
                                uqa_execution::PhysicalRow::from_shared_values(values, projection)
                            }
                            None => uqa_execution::PhysicalRow::nulls(fields.len()),
                        },
                        doc_id,
                    )?);
                }
            } else {
                let mut visited = 0usize;
                let mut predicate_error = None;
                store
                    .for_each_fields_multi_ref(&doc_ids, &fields, &mut |doc_id, values| {
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
                        match self.with_lock_identity(
                            uqa_execution::PhysicalRow::from_values(
                                values.iter().map(|value| (*value).clone()).collect(),
                            ),
                            doc_id,
                        ) {
                            Ok(row) => rows.push(row),
                            Err(error) => {
                                predicate_error = Some(error);
                                return false;
                            }
                        }
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

    /// Emit exactly the candidate tuples pinned for a tuple-local row-lock recheck: the latest committed image for a changed tuple, or the statement-snapshot image for an unchanged join partner. Pushed predicates re-apply to the substituted values, matching `PostgreSQL`'s `EvalPlanQual` scan behavior for marked relations.
    fn next_pinned_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> uqa_execution::ExecResult<Vec<uqa_execution::PhysicalRow>> {
        let Some(pins) = self.recheck_pins.as_ref() else {
            return Err(
                SQLError::Internal("row-lock recheck scan has no pinned tuples".into()).into(),
            );
        };
        let pins = Arc::clone(pins);
        let store = self.table.document_store.read();
        let mut rows = Vec::with_capacity(max_rows.min(pins.len()));
        while rows.len() < max_rows && self.recheck_cursor < pins.len() {
            let pin = &pins[self.recheck_cursor];
            self.recheck_cursor += 1;
            let mut document = if let Some(document) = pin.document.as_ref() {
                (**document).clone()
            } else {
                let Some(document) = store.get(pin.doc_id).map_err(|error| {
                    SQLError::Internal(format!(
                        "read pinned recheck row from `{}`: {error}",
                        self.table_name
                    ))
                })?
                else {
                    continue;
                };
                document
            };
            crate::engine_generated::materialize_projected_virtual_generated_columns(
                &self.column_definitions,
                &mut document,
                &self.columns,
            )?;
            let values = self
                .columns
                .iter()
                .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>();
            let value_refs = values.iter().collect::<Vec<_>>();
            if let Some(predicate) = self.predicate.as_ref() {
                if !predicate.keep(&value_refs)? {
                    continue;
                }
            }
            rows.push(
                self.with_lock_identity(
                    uqa_execution::PhysicalRow::from_values(values),
                    pin.doc_id,
                )?,
            );
        }
        Ok(rows)
    }

    fn next_virtual_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> uqa_execution::ExecResult<Vec<uqa_execution::PhysicalRow>> {
        let store = self.table.document_store.read();
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            let remaining = max_rows - rows.len();
            let doc_ids = store.next_doc_ids(self.after, remaining).map_err(|error| {
                SQLError::Internal(format!(
                    "scan generated rows from `{}`: {error}",
                    self.table_name
                ))
            })?;
            let Some(last) = doc_ids.last().copied() else {
                break;
            };
            self.after = Some(last);
            let mut documents = store.get_many(&doc_ids).map_err(|error| {
                SQLError::Internal(format!(
                    "read generated rows from `{}`: {error}",
                    self.table_name
                ))
            })?;
            for doc_id in doc_ids {
                let mut document = documents.remove(&doc_id).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "table `{}` listed document {doc_id} but did not return it",
                        self.table_name
                    ))
                })?;
                crate::engine_generated::materialize_projected_virtual_generated_columns(
                    &self.column_definitions,
                    &mut document,
                    &self.columns,
                )?;
                let values = self
                    .columns
                    .iter()
                    .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                let value_refs = values.iter().collect::<Vec<_>>();
                if let Some(predicate) = self.predicate.as_ref() {
                    if !predicate.keep(&value_refs)? {
                        continue;
                    }
                }
                rows.push(
                    self.with_lock_identity(
                        uqa_execution::PhysicalRow::from_values(values),
                        doc_id,
                    )?,
                );
            }
        }
        Ok(rows)
    }

    fn with_lock_identity(
        &self,
        row: uqa_execution::PhysicalRow,
        doc_id: uqa_core::DocId,
    ) -> Result<uqa_execution::PhysicalRow, SQLError> {
        let Some((qualifier, storage_name)) = self.lock_origin.as_ref() else {
            return Ok(row);
        };
        Ok(
            row.with_lock_origin(uqa_execution::RowLockOrigin::from_shared(
                std::sync::Arc::clone(qualifier),
                std::sync::Arc::clone(storage_name),
                doc_id,
            )),
        )
    }
}

impl uqa_execution::RowSource for EngineTableRowSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn physical_schema(&self) -> Option<&uqa_execution::RowSchema> {
        Some(&self.physical_schema)
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        Some(self.estimated_cardinality)
    }

    fn next_row(&mut self) -> uqa_execution::ExecResult<Option<ResultRow>> {
        Ok(self.next_batch(1)?.pop())
    }

    fn next_batch(&mut self, max_rows: usize) -> uqa_execution::ExecResult<Vec<ResultRow>> {
        let rows = self.next_physical_rows_batch(max_rows)?;
        Ok(rows
            .iter()
            .map(|row| self.physical_schema.view(row).to_result_row())
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
    let SourcePlan::Table {
        name,
        qualifier,
        alias,
    } = source
    else {
        return Ok(None);
    };
    let qualifier = qualifier_for(qualifier, alias.as_deref());
    if let Some(materialized) = ctes.rows.get(name).cloned() {
        if has_filters_for_qualifier(filters, &qualifier) {
            return Ok(None);
        }
        let mapping = materialized
            .row_schema()
            .identities()
            .iter()
            .enumerate()
            .filter_map(|(position, identity)| {
                let column = identity.column();
                if !is_score_provenance_column(column)
                    && prune
                        .and_then(|prune| prune.get(&qualifier))
                        .is_some_and(|wanted| !wanted.contains(column))
                {
                    return None;
                }
                let output_identity = uqa_execution::ColumnIdentity::qualified(&qualifier, column);
                Some((column.to_string(), output_identity, position))
            })
            .collect();
        let scan: Box<dyn uqa_execution::PhysicalOperator + 'a> =
            Box::new(uqa_execution::SharedSpillScan::new(materialized));
        let selection = uqa_execution::ColumnSelection::with_identities(scan, mapping);
        let selection = if ctes.lock_identities.emit {
            selection.rebinding_lock_origins(qualifier)
        } else {
            selection
        };
        return Ok(Some((Box::new(selection), false)));
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
    let schema = columns.clone();
    let column_definitions = table.columns.read().clone();
    let column_types = columns
        .iter()
        .map(|column| {
            column_definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    let lock_origin = table_lock_origin(engine, name, &qualifier, ctes.lock_identities.emit)?;
    let physical_schema =
        uqa_execution::RowSchema::with_qualified_types(&qualifier, schema.clone(), column_types);
    let predicate = qualifier_filter(filters, &qualifier)
        .map(|predicate| {
            uqa_execution::ProjectedPredicate::compile_with_schema(
                &predicate,
                &physical_schema,
                params,
            )
        })
        .transpose()?
        .flatten();
    let filter_pushed = predicate.is_some();
    let recheck_pins = lock_origin
        .as_ref()
        .and_then(|(origin_qualifier, storage_name)| {
            ctes.recheck_docs_for_scan(origin_qualifier, storage_name)
        });
    let command_changes = if ctes.reads_command_overlay() {
        engine.command_overlay_changes(name)?.map(Arc::new)
    } else {
        None
    };
    let estimated_cardinality = engine.table_doc_count(name)?;
    let source = EngineTableRowSource {
        table_name: name.clone(),
        table,
        column_definitions,
        columns,
        schema,
        physical_schema,
        predicate,
        estimated_cardinality,
        after: None,
        lock_origin,
        recheck_pins,
        recheck_cursor: 0,
        command_changes,
        command_change_after: None,
        command_base_after: None,
        command_base_ids: std::collections::VecDeque::new(),
        command_base_exhausted: false,
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
    build_join_operator_with_ctes_at_path(engine, from, params, ctes, prune, filters, None)
}

pub(in crate::sql) fn build_join_operator_with_recheck_pins<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    build_join_operator_with_ctes_at_path(
        engine,
        from,
        params,
        ctes,
        prune,
        filters,
        Some(Vec::new()),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_join_operator_with_ctes_at_path<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    recheck_path: Option<Vec<u8>>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::{HashJoin, LateralJoin, NestedLoopJoin, PhysicalOperator};

    if let Some(source) = recheck_path
        .as_deref()
        .and_then(|path| ctes.recheck_source_row(path))
    {
        let scan: Box<dyn PhysicalOperator + 'a> = Box::new(
            uqa_execution::TableScan::from_physical_rows(source.schema, vec![source.row]),
        );
        if matches!(from, SourcePlan::Values { .. })
            || qualifier_filter(filters, &source.qualifier)
                .is_some_and(|predicate| uqa_planner::optimizer::contains_retrieval(&predicate))
        {
            return Ok(scan);
        }
        return Ok(attach_qualifier_filter(
            scan,
            &source.qualifier,
            filters,
            engine,
            params,
            ctes,
        ));
    }

    match from {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
        } => {
            let qualifier = qualifier_for(qualifier, alias.as_deref());
            if let Some(materialized) = ctes.rows.get(name).cloned() {
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(materialized));
                let operator =
                    qualify_source_operator(scan, &qualifier, prune, ctes.lock_identities.emit);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(plan) = ctes.remove_deferred(name) {
                let streamed = {
                    let mut scoped_ctes = ctes.enter_lock_identity_emission(false);
                    try_build_streaming_subquery_operator(
                        engine,
                        &plan.query,
                        params,
                        &mut scoped_ctes,
                    )?
                };
                if let Some(operator) = streamed {
                    let source_columns = operator.schema().to_vec();
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        &qualifier,
                        prune,
                        &plan.columns,
                        false,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, engine, params, ctes,
                    ));
                }
                materialize_plan_ctes(engine, std::slice::from_ref(&plan), params, ctes)?;
                let materialized = ctes.rows.get(name).cloned().ok_or_else(|| {
                    SQLError::Internal(format!(
                        "deferred CTE `{name}` did not produce a materialized input"
                    ))
                })?;
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(materialized));
                let operator = qualify_source_operator(scan, &qualifier, prune, false);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(plan) = engine.view_plan(name)? {
                let inherited_lock = ctes.source_row_lock_for_view(&qualifier, name);
                // During a tuple-local recheck, a view named as the lock target pins every base scan of its storage inside this subtree to the candidate's tuples.
                let mut recheck_scope = ctes.enter_recheck_storage_pins(&qualifier);
                let ctes: &mut CteScope = &mut recheck_scope;
                let specialized_plan = filters
                    .and_then(|filters| filters.get(&qualifier))
                    .filter(|filters| !filters.is_empty())
                    .and_then(|filters| combine_filters(filters.iter().cloned()))
                    .map(|filter| {
                        push_output_filter_into_query_plan(engine, &plan, &qualifier, &filter, None)
                    })
                    .transpose()?
                    .flatten();
                let propagated_plan = inherited_lock.as_ref().map(|target| {
                    let mut plan = specialized_plan.clone().unwrap_or_else(|| plan.clone());
                    apply_propagated_view_lock(&mut plan, target);
                    plan
                });
                let execution_plan = propagated_plan
                    .as_ref()
                    .or(specialized_plan.as_ref())
                    .unwrap_or(&plan);
                if let Some(operator) =
                    try_build_streaming_subquery_operator(engine, execution_plan, params, ctes)?
                {
                    let source_columns = operator.schema().to_vec();
                    let operator = qualify_source_operator_with_columns(
                        operator,
                        &source_columns,
                        &qualifier,
                        prune,
                        &[],
                        ctes.lock_identities.emit,
                    );
                    return Ok(attach_qualifier_filter(
                        operator, &qualifier, filters, engine, params, ctes,
                    ));
                }
                let local_cte_names = query_cte_names(execution_plan);
                let is_volatile = query_contains_volatile_function(engine, execution_plan)?;
                let output = if is_volatile || propagated_plan.is_some() {
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
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(shared));
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    &qualifier,
                    prune,
                    &[],
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(rows) = build_info_schema_rows(engine, name)? {
                let schema = virtual_relation_schema(engine, name).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "virtual relation `{name}` has rows but no PostgreSQL 18 row type"
                    ))
                })?;
                let (columns, types): (Vec<_>, Vec<_>) = schema
                    .into_iter()
                    .map(|(column, ty)| (column, Some(ty)))
                    .unzip();
                let scan: Box<dyn PhysicalOperator + 'a> = Box::new(
                    uqa_execution::TableScan::from_typed_rows(columns.clone(), types, rows),
                );
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    &qualifier,
                    prune,
                    &[],
                    ctes.lock_identities.emit,
                );
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
                let typed_columns = engine
                    .foreign_table_typed_columns(name)
                    .map_err(SQLError::Unsupported)?;
                let columns = typed_columns
                    .iter()
                    .map(|(column, _)| column.clone())
                    .collect::<Vec<_>>();
                let types = typed_columns.into_iter().map(|(_, ty)| Some(ty)).collect();
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::RowIteratorScan::with_types(
                        columns.clone(),
                        types,
                        Box::new(rows.map(|row| {
                            row.map_err(SQLError::Unsupported)
                                .map_err(uqa_execution::ExecError::from)
                        })),
                    ));
                let operator = qualify_source_operator_with_columns(
                    scan,
                    &columns,
                    &qualifier,
                    prune,
                    &[],
                    ctes.lock_identities.emit,
                );
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(predicate) = qualifier_filter(filters, &qualifier)
                .filter(uqa_planner::optimizer::contains_retrieval)
            {
                let lock_origin =
                    table_lock_origin(engine, name, &qualifier, ctes.lock_identities.emit)?;
                let recheck_pins =
                    lock_origin
                        .as_ref()
                        .and_then(|(origin_qualifier, storage_name)| {
                            ctes.recheck_docs_for_scan(origin_qualifier, storage_name)
                        });
                // A tuple-local recheck judges the substituted committed images with the retrieval predicate re-executed against the latest committed index state, not this session's pinned snapshot.
                let entries = if recheck_pins.is_some() {
                    engine.committed_retrieval_entries(name, &predicate, params)?
                } else {
                    crate::operator_tree_bridge::run_optimised(
                        engine,
                        name,
                        Some(&predicate),
                        params,
                    )?
                }
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
                )
                .with_lock_origin(lock_origin)
                .with_recheck_pins(recheck_pins);
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::TableScan::new(Box::new(source)));
                return Ok(qualify_source_operator(
                    scan,
                    &qualifier,
                    prune,
                    ctes.lock_identities.emit,
                ));
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
            using,
            natural,
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
                return if let Some(using) = resolved_using.as_ref() {
                    shape_join_using_output(joined, *kind, &left_schema, &right_schema, using)
                } else {
                    Ok(joined)
                };
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
            let work_mem = physical_work_mem_bytes(engine)?;
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
            if let Some(using) = resolved_using.as_ref() {
                shape_join_using_output(joined, *kind, &left_schema, &right_schema, using)
            } else {
                Ok(joined)
            }
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let column_types = crate::sql::select::values_types_in_scope(
                engine,
                rows,
                &ctes.scalar_subqueries,
                None,
                params,
                ctes,
            )?;
            let source_columns = if column_aliases.is_empty() {
                (0..rows.first().map_or(0, Vec::len))
                    .map(|index| format!("column{}", index + 1))
                    .collect::<Vec<_>>()
            } else {
                column_aliases.clone()
            };
            let hook = ScopedEngineHook::new(engine, ctes);
            let context =
                SourceEvalContext::new(engine, params, &hook, &hook, &ctes.scalar_subqueries);
            let rows = build_values_physical_rows(&context, rows, &column_types)?;
            let schema = uqa_execution::RowSchema::with_types(source_columns.clone(), column_types);
            let operator: Box<dyn uqa_execution::PhysicalOperator + 'a> =
                Box::new(uqa_execution::TableScan::from_physical_rows(schema, rows));
            Ok(qualify_source_operator_with_columns(
                operator,
                &source_columns,
                alias.as_deref().unwrap_or_default(),
                prune,
                &[],
                ctes.lock_identities.emit,
            ))
        }
        SourcePlan::Function {
            name,
            output_name,
            relation,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        } => {
            let bound_columns = crate::sql::select::user_function_output_columns(engine, name)
                .map_or_else(
                    || {
                        table_function_empty_schema(
                            name,
                            output_name,
                            alias.as_deref(),
                            column_aliases,
                            args.len(),
                            *ordinality,
                        )
                    },
                    |columns| apply_table_function_aliases(columns, column_aliases, *ordinality),
                );
            validate_table_function_alias_count(
                alias.as_deref().unwrap_or(output_name),
                bound_columns.len(),
                column_aliases.len(),
            )?;
            let hook = ScopedEngineHook::new(engine, ctes);
            let context =
                SourceEvalContext::new(engine, params, &hook, &hook, &ctes.scalar_subqueries);
            let call = TableFunctionCall {
                name,
                output_name,
                relation: relation.as_deref(),
                args,
                alias: alias.as_deref(),
                column_aliases,
                ordinality: *ordinality,
                column_types,
            };
            let rows = build_table_function_row_stream(&context, call)?;
            let multi_unnest =
                crate::sql::builtin_function_dispatch_name(name) == "unnest" && args.len() > 1;
            let (operator, source_columns): (
                Box<dyn uqa_execution::PhysicalOperator + 'a>,
                Vec<String>,
            ) = if multi_unnest {
                let public_columns = bound_columns.clone();
                let mut internal_columns = multi_unnest_internal_columns(args.len());
                if *ordinality {
                    internal_columns.push(TABLE_FUNCTION_ORDINALITY_COLUMN.into());
                }
                let types = table_function_column_types(
                    engine,
                    TableFunctionTypeRequest {
                        name,
                        args,
                        declared_types: column_types,
                        columns: &public_columns,
                        ordinality: *ordinality,
                    },
                    &uqa_execution::RowSchema::default(),
                    params,
                );
                let identities = public_columns
                    .into_iter()
                    .map(uqa_execution::ColumnIdentity::unqualified)
                    .collect();
                let schema = uqa_execution::RowSchema::with_identities(
                    internal_columns.clone(),
                    identities,
                    types,
                );
                (
                    Box::new(uqa_execution::RowIteratorScan::with_row_schema(
                        schema,
                        Box::new(rows),
                    )),
                    internal_columns,
                )
            } else {
                let mut rows = rows;
                let first = rows
                    .next()
                    .transpose()
                    .map_err(crate::sql::select::physical_exec_error)?;
                let columns = if *ordinality {
                    first.as_ref().map_or(bound_columns.clone(), |row| {
                        if row.len() == bound_columns.len() {
                            return bound_columns.clone();
                        }
                        let mut columns = row
                            .keys()
                            .filter(|column| column.as_str() != TABLE_FUNCTION_ORDINALITY_COLUMN)
                            .cloned()
                            .collect::<Vec<_>>();
                        let ordinality_column = column_aliases
                            .get(columns.len())
                            .cloned()
                            .unwrap_or_else(|| "ordinality".into());
                        columns.push(ordinality_column);
                        columns
                    })
                } else if column_aliases.is_empty() {
                    first.as_ref().map_or_else(
                        || {
                            table_function_empty_schema(
                                name,
                                output_name,
                                alias.as_deref(),
                                column_aliases,
                                args.len(),
                                false,
                            )
                        },
                        |row| row.keys().cloned().collect(),
                    )
                } else {
                    table_function_empty_schema(
                        name,
                        output_name,
                        alias.as_deref(),
                        column_aliases,
                        args.len(),
                        false,
                    )
                };
                let rows = first.into_iter().map(Ok).chain(rows);
                let types = table_function_column_types(
                    engine,
                    TableFunctionTypeRequest {
                        name,
                        args,
                        declared_types: column_types,
                        columns: &columns,
                        ordinality: *ordinality,
                    },
                    &uqa_execution::RowSchema::default(),
                    params,
                );
                if *ordinality {
                    let mut internal_columns = columns.clone();
                    if let Some(column) = internal_columns.last_mut() {
                        *column = TABLE_FUNCTION_ORDINALITY_COLUMN.into();
                    }
                    let identities = columns
                        .into_iter()
                        .map(uqa_execution::ColumnIdentity::unqualified)
                        .collect();
                    let schema = uqa_execution::RowSchema::with_identities(
                        internal_columns.clone(),
                        identities,
                        types,
                    );
                    (
                        Box::new(uqa_execution::RowIteratorScan::with_row_schema(
                            schema,
                            Box::new(rows),
                        )),
                        internal_columns,
                    )
                } else {
                    (
                        Box::new(uqa_execution::RowIteratorScan::with_types(
                            columns.clone(),
                            types,
                            Box::new(rows),
                        )),
                        columns,
                    )
                }
            };
            let qualifier = alias.as_deref().unwrap_or(output_name);
            let operator = qualify_source_operator_with_columns(
                operator,
                &source_columns,
                qualifier,
                prune,
                &[],
                ctes.lock_identities.emit,
            );
            Ok(attach_qualifier_filter(
                operator, qualifier, filters, engine, params, ctes,
            ))
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            // During a tuple-local recheck, a derived table named as the lock target pins every base scan of its storage inside this subtree to the candidate's tuples.
            let visible_qualifier = alias.clone().unwrap_or_default();
            let mut recheck_scope = ctes.enter_recheck_storage_pins(&visible_qualifier);
            let ctes: &mut CteScope = &mut recheck_scope;
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
                    ctes.lock_identities.emit,
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
                ctes.lock_identities.emit,
            );
            Ok(attach_qualifier_filter(
                operator, qualifier, filters, engine, params, ctes,
            ))
        }
    }
}

/// Build a single-consumer derived-table projection as a pull pipeline. Blocking operators inside the query block retain their own bounded state, but a second `SharedSpill` boundary would eagerly exhaust that pipeline before the parent can apply demand such as `LIMIT`.
fn try_build_streaming_subquery_operator<'a>(
    engine: &'a Engine,
    body: &uqa_planner::QueryPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
) -> Result<Option<Box<dyn uqa_execution::PhysicalOperator + 'a>>, SQLError> {
    if !body.ctes.is_empty()
        || (!ctes.streams_command_progress() && query_contains_volatile_function(engine, body)?)
    {
        return Ok(None);
    }
    let RelationalPlan::QueryBlock(block) = &body.root else {
        return Ok(None);
    };
    // A block whose qualification calls a registered retrieval function (text_match, knn_match, graph_traverse, rpq, ...) executes it through the operator-tree bridge of the single-table executor; the residual scalar filter of a streamed block cannot evaluate such calls. Plain comparisons keep streaming so an outer LIMIT still bounds locking demand inside the derived table.
    if !matches!(block.compute, ComputePlan::Project)
        || matches!(block.access, AccessPathPlan::Hybrid)
        || block
            .r#where
            .as_ref()
            .is_some_and(uqa_planner::optimizer::contains_retrieval)
        || block.from.is_none()
        || block.distinct
        || !block.distinct_on.is_empty()
    {
        return Ok(None);
    }

    let from = block
        .from
        .as_ref()
        .expect("derived-table FROM checked above");
    // The block's scalar subqueries live in their own arena for the whole pull pipeline: the evaluators built below snapshot this scope, so a derived table with subqueries still streams and an outer LIMIT keeps its inner locking demand-driven.
    let mut ctes = ctes.enter_scalar_subqueries(&block.subqueries);
    let ctes: &mut CteScope = &mut ctes;
    let source_schema = bind_source_plan_schema(engine, from, params, ctes, None)?;
    let projections = crate::sql::select::physical_projections(&block.projections);
    if crate::sql::select::projections_may_return_set(engine, &projections, &source_schema, params)?
    {
        return Ok(None);
    }
    let (_, order_output) =
        crate::sql::select::order_projection(&block.projections, &source_schema)?;
    for order in &block.order_by {
        let expression = crate::sql::select::resolve_order_expression(&order.expr, &order_output)?;
        if crate::sql::select::expression_may_return_set(
            engine,
            &expression,
            &source_schema,
            params,
        )? {
            return Ok(None);
        }
    }

    let emit_lock_identities = ctes.lock_identities.emit || !block.locking.is_empty();
    let previous_lock_identities = ctes.lock_identities;
    ctes.lock_identities.emit = emit_lock_identities;
    ctes.lock_identities.retain_after_lock = previous_lock_identities.emit;
    let result = (|| {
        let column_prune = crate::sql::select::column_prune_for_stmt(engine, block, from);
        let qualifier_filters = crate::sql::select::qualifier_filters_for_stmt(engine, block, from);
        let source_row_locks = resolve_row_locks(
            engine,
            from,
            &block.locking,
            block.r#where.as_ref(),
            params,
            ctes,
        )?;
        let operator = {
            let mut scoped_ctes = ctes.enter_source_row_locks(source_row_locks);
            build_join_operator_with_ctes(
                engine,
                from,
                params,
                &mut scoped_ctes,
                column_prune.as_ref(),
                qualifier_filters.as_ref(),
            )?
        };
        let residual = crate::sql::select::final_filter_after_qualifier_pushdown(
            engine,
            block,
            from,
            qualifier_filters.as_ref(),
        );
        let operator = crate::sql::select::build_relational_operator(
            engine, operator, residual, block, params, ctes,
        )?;
        Ok(Some(operator))
    })();
    ctes.lock_identities = previous_lock_identities;
    result
}
