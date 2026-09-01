//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    attach_order_limit, bind_query_plan_schema, close_after_physical_failure, cte_output_filters,
    cte_references_own_name, execute_plan_values_output, execute_query_block_output,
    identity_order_columns, materialize_plan_ctes_with_filters, ordered_plan_ctes,
    physical_exec_error, physical_work_mem_bytes, query_contains_volatile_function,
    reachable_plan_cte_names, resolve_limit_offset_with_ctes, single_reference_plan_cte_names,
    validate_values_set_contexts, AccessPathPlan, BTreeSet, ComputePlan, CteScope, DirectColumnKey,
    DirectionalQueryPlanOperator, Engine, EngineExpressionEvaluator, ProjectionPlan,
    QueryBlockPlan, QueryConsumerControl, QueryOutput, QueryOutputMode, QueryPlan, QueryRows, Rc,
    RelationalPlan, SQLError, SQLParam, SQLResult, ScopedEngineHook, SetOpKind,
    SetOperationRowConsumer, SharedExpressionEvaluator, SmallVec, Value,
};

pub(in crate::sql) fn execute_query_plan_with_ctes(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    execute_query_plan_output(engine, plan, params, ctes, QueryOutputMode::Rows)?.into_sql_result()
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(in crate::sql) fn execute_query_plan_output(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let mut visible_ctes = ctes.enter_visible_ctes(plan.ctes.iter().map(|cte| cte.name.as_str()));
    let ctes = &mut *visible_ctes;
    if !plan.ctes.is_empty() {
        let ordered_ctes = ordered_plan_ctes(plan)?;
        let reachable = reachable_plan_cte_names(plan);
        let single_reference = single_reference_plan_cte_names(plan);
        let recursive = ordered_ctes
            .iter()
            .copied()
            .filter(|cte| cte_references_own_name(cte))
            .map(|cte| cte.name.as_str())
            .collect::<BTreeSet<_>>();
        for cte in ordered_ctes.iter().copied().filter(|cte| {
            !recursive.contains(cte.name.as_str())
                && reachable.contains(&cte.name)
                && match cte.materialization {
                    uqa_sql::ast::CteMaterialization::Default => {
                        single_reference.contains(&cte.name)
                    }
                    uqa_sql::ast::CteMaterialization::Materialized => false,
                    uqa_sql::ast::CteMaterialization::NotMaterialized => true,
                }
                && matches!(
                    query_contains_volatile_function(engine, &cte.query),
                    Ok(false)
                )
        }) {
            ctes.insert_deferred(cte.clone());
        }
        let filters = cte_output_filters(engine, plan, ctes)?;
        materialize_plan_ctes_with_filters(
            engine,
            ordered_ctes.into_iter().filter(|cte| {
                reachable.contains(&cte.name)
                    && (recursive.contains(cte.name.as_str())
                        || matches!(
                            cte.materialization,
                            uqa_sql::ast::CteMaterialization::Materialized
                        )
                        || (matches!(
                            cte.materialization,
                            uqa_sql::ast::CteMaterialization::Default
                        ) && !single_reference.contains(&cte.name))
                        || !matches!(
                            query_contains_volatile_function(engine, &cte.query),
                            Ok(false)
                        ))
            }),
            params,
            ctes,
            &filters,
        )?;
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            execute_query_block_output(engine, block, params, ctes, output_mode)
        }
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            with_ties,
            offset,
            subqueries,
        } => {
            let set_schema = bind_query_plan_schema(engine, plan, params, ctes, None)?;
            let directional_union_all = matches!(
                &output_mode,
                QueryOutputMode::RowConsumer(downstream)
                    if downstream.uses_directional_scan()
                        && matches!((*kind, *all), (SetOpKind::Union, true))
                        && order_by.is_empty()
                        && !*with_ties
            );
            if directional_union_all {
                let left_schema = bind_query_plan_schema(engine, left, params, ctes, None)?;
                let right_schema = bind_query_plan_schema(engine, right, params, ctes, None)?;
                let child_ctes = {
                    let child_scope = ctes.enter_lock_identity_emission(false);
                    (*child_scope).clone()
                };
                let left: Box<dyn uqa_execution::PhysicalOperator + '_> = Box::new(
                    DirectionalQueryPlanOperator::new(
                        engine.fork_session_portal_worker_engine()?,
                        (**left).clone(),
                        params.to_vec(),
                        child_ctes.clone(),
                        left_schema,
                    )
                    .map_err(physical_exec_error)?,
                );
                let right: Box<dyn uqa_execution::PhysicalOperator + '_> = Box::new(
                    DirectionalQueryPlanOperator::new(
                        engine.fork_session_portal_worker_engine()?,
                        (**right).clone(),
                        params.to_vec(),
                        child_ctes,
                        right_schema,
                    )
                    .map_err(physical_exec_error)?,
                );
                let mut operation: Box<dyn uqa_execution::PhysicalOperator + '_> = Box::new(
                    uqa_execution::ExternalSetOperation::new_directional_with_types(
                        left,
                        right,
                        *kind,
                        *all,
                        set_schema.column_types().to_vec(),
                        physical_work_mem_bytes(engine.query_runtime_view())?,
                    )
                    .map_err(physical_exec_error)?,
                );
                let (resolved_offset, resolved_limit) = {
                    let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
                    (
                        resolve_limit_offset_with_ctes(
                            offset.as_deref(),
                            engine,
                            params,
                            "OFFSET",
                            &scoped_ctes,
                        )?
                        .unwrap_or(0),
                        resolve_limit_offset_with_ctes(
                            limit.as_deref(),
                            engine,
                            params,
                            "LIMIT",
                            &scoped_ctes,
                        )?,
                    )
                };
                if resolved_offset != 0 || resolved_limit.is_some() {
                    operation = Box::new(uqa_execution::Limit::new(
                        operation,
                        resolved_offset,
                        resolved_limit,
                    ));
                }
                return collect_query_operator(
                    engine,
                    set_schema.columns().to_vec(),
                    operation,
                    output_mode,
                );
            }
            let streaming_consumer = match &output_mode {
                QueryOutputMode::RowConsumer(downstream)
                    if matches!((*kind, *all), (SetOpKind::Union, true))
                        && order_by.is_empty()
                        && !*with_ties =>
                {
                    Some(Rc::clone(downstream))
                }
                _ => None,
            };
            if let Some(downstream) = streaming_consumer {
                let columns = set_schema.columns().to_vec();
                let column_types = set_schema.column_types().to_vec();
                let (resolved_offset, resolved_limit) = {
                    let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
                    (
                        resolve_limit_offset_with_ctes(
                            offset.as_deref(),
                            engine,
                            params,
                            "OFFSET",
                            &scoped_ctes,
                        )?
                        .unwrap_or(0),
                        resolve_limit_offset_with_ctes(
                            limit.as_deref(),
                            engine,
                            params,
                            "LIMIT",
                            &scoped_ctes,
                        )?,
                    )
                };
                let consumer = Rc::new(SetOperationRowConsumer::new(
                    Rc::clone(&downstream),
                    set_schema.clone(),
                    resolved_offset,
                    resolved_limit,
                ));
                if consumer.stopped() {
                    downstream.begin(engine, &columns, &set_schema)?;
                } else {
                    let mut child_ctes = ctes.enter_lock_identity_emission(false);
                    execute_query_plan_output(
                        engine,
                        left,
                        params,
                        &mut child_ctes,
                        QueryOutputMode::RowConsumer(consumer.clone()),
                    )?;
                    if !consumer.stopped() {
                        execute_query_plan_output(
                            engine,
                            right,
                            params,
                            &mut child_ctes,
                            QueryOutputMode::RowConsumer(consumer),
                        )?;
                    }
                }
                return Ok(QueryOutput {
                    columns: columns.clone(),
                    column_types: column_types.clone(),
                    internal_columns: columns,
                    internal_types: column_types,
                    rows: QueryRows::Rows {
                        named: Vec::new(),
                        positional: None,
                    },
                });
            }
            // Materialize each child directly into a disk-backed, repeatable stream before starting the next child. A nested set operation therefore never owns two cardinality-sized `SQLResult.rows` vectors, and its external merge consumes batches under `work_mem`.
            let (lhs, rhs) = {
                let mut child_ctes = ctes.enter_lock_identity_emission(false);
                let lhs = execute_query_plan_output(
                    engine,
                    left,
                    params,
                    &mut child_ctes,
                    QueryOutputMode::SharedSpill,
                )?;
                let rhs = execute_query_plan_output(
                    engine,
                    right,
                    params,
                    &mut child_ctes,
                    QueryOutputMode::SharedSpill,
                )?;
                (lhs, rhs)
            };
            let columns = lhs.columns.clone();
            let left: Box<dyn uqa_execution::PhysicalOperator + '_> = lhs.into_public_operator();
            let right: Box<dyn uqa_execution::PhysicalOperator + '_> = rhs.into_public_operator();
            let operation: Box<dyn uqa_execution::PhysicalOperator + '_> = Box::new(
                uqa_execution::ExternalSetOperation::new_with_types(
                    left,
                    right,
                    *kind,
                    *all,
                    set_schema.column_types().to_vec(),
                    physical_work_mem_bytes(engine.query_runtime_view())?,
                )
                .map_err(physical_exec_error)?,
            );
            if !order_by.is_empty() || limit.is_some() || offset.is_some() {
                let synthetic = QueryBlockPlan {
                    projections: Vec::new(),
                    from: None,
                    r#where: None,
                    compute: ComputePlan::Project,
                    group_by: Vec::new(),
                    grouping_sets: Vec::new(),
                    group_distinct: false,
                    having: None,
                    order_by: order_by.clone(),
                    limit: limit.as_deref().cloned(),
                    with_ties: *with_ties,
                    offset: offset.as_deref().cloned(),
                    distinct: false,
                    distinct_on: Vec::new(),
                    subqueries: subqueries.clone(),
                    access: AccessPathPlan::Row,
                    locking: Vec::new(),
                };
                let ordering_scope = ctes.enter_scalar_subqueries(subqueries);
                let evaluator = EngineExpressionEvaluator::shared(engine, params, &ordering_scope);
                let output = identity_order_columns(&columns);
                let operation = attach_order_limit(
                    operation,
                    &synthetic,
                    &output,
                    engine,
                    params,
                    &ordering_scope,
                    engine.query_runtime_view(),
                    evaluator,
                    None,
                )?;
                return collect_query_operator(engine, columns, operation, output_mode);
            }
            collect_query_operator(engine, columns, operation, output_mode)
        }
        RelationalPlan::Values { rows, subqueries } => {
            {
                let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
                let type_resolver = ScopedEngineHook::new(engine, &scoped_ctes);
                validate_values_set_contexts(
                    engine,
                    &type_resolver,
                    rows,
                    &uqa_execution::RowSchema::default(),
                    params,
                )?;
            }
            execute_plan_values_output(engine, rows, subqueries, params, ctes, output_mode)
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(in crate::sql) fn collect_query_operator<'a>(
    engine: &Engine,
    columns: Vec<String>,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let internal_schema = operator.row_schema().clone();
    let internal_columns = internal_schema.columns().to_vec();
    let internal_types = internal_schema.column_types().to_vec();
    let column_types = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            if internal_schema.columns().get(index) == Some(column) {
                internal_schema.column_type(index).cloned()
            } else {
                internal_schema
                    .position(column)
                    .and_then(|position| internal_schema.column_type(position).cloned())
            }
        })
        .collect();
    let rows = match output_mode {
        QueryOutputMode::Rows => {
            let has_duplicate_labels = {
                let mut seen = std::collections::BTreeSet::new();
                columns.iter().any(|column| !seen.insert(column))
            };
            if has_duplicate_labels {
                let batches = uqa_execution::physical::run_to_batches(operator.as_mut())
                    .map_err(physical_exec_error)?;
                let mut named = Vec::new();
                let mut positional = Vec::new();
                for batch in batches {
                    let columnar =
                        uqa_execution::ColumnarBatch::from_batch(&columns, batch.clone());
                    positional.extend(columnar.into_positional_rows());
                    named.extend(batch.into_result_rows());
                }
                QueryRows::Rows {
                    named,
                    positional: Some(positional),
                }
            } else {
                QueryRows::Rows {
                    named: uqa_execution::physical::run_to_rows(operator.as_mut())
                        .map_err(physical_exec_error)?
                        .1,
                    positional: None,
                }
            }
        }
        QueryOutputMode::SharedSpill => {
            let mut buffer = uqa_execution::SpillBuffer::new(
                physical_work_mem_bytes(engine.query_runtime_view())?.max(1),
            );
            if let Err(error) = operator.open() {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "open",
                ));
            }
            loop {
                let batch = match operator.next() {
                    Ok(batch) => batch,
                    Err(error) => {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "execution",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                if let Err(error) = buffer.push(batch) {
                    return Err(close_after_physical_failure(
                        operator.as_mut(),
                        error,
                        "spill buffering",
                    ));
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::SharedSpill(
                buffer
                    .into_shared(internal_schema)
                    .map_err(physical_exec_error)?,
            )
        }
        QueryOutputMode::ExistsKeySet => {
            if operator.row_schema().len() < columns.len() {
                return Err(SQLError::Internal(format!(
                    "decorrelated EXISTS result has {} columns for {} keys",
                    operator.row_schema().len(),
                    columns.len()
                )));
            }
            let key_positions = (0..columns.len()).collect::<Vec<_>>();
            let mut keys = uqa_execution::CanonicalRowHashSet::new();
            if let Err(error) = operator.open() {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "open EXISTS key input",
                ));
            }
            loop {
                let batch = match operator.next() {
                    Ok(batch) => batch,
                    Err(error) => {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "collect EXISTS keys",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                for row in &batch.rows {
                    let view = batch.schema.view(row);
                    let mut key = SmallVec::<[&Value; 4]>::with_capacity(key_positions.len());
                    let mut contains_null = false;
                    for position in &key_positions {
                        let Some(value) = view.value_at(*position) else {
                            contains_null = true;
                            break;
                        };
                        if matches!(value, Value::Null) {
                            contains_null = true;
                            break;
                        }
                        key.push(value);
                    }
                    if !contains_null {
                        if let Err(error) = keys.insert_borrowed(&key) {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                error,
                                "hash EXISTS keys",
                            ));
                        }
                    }
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::ExistsKeySet(keys)
        }
        QueryOutputMode::RowConsumer(consumer) => {
            if consumer.uses_directional_scan() {
                collect_directional_query_operator(engine, &columns, &mut operator, &consumer)?;
                return Ok(QueryOutput {
                    columns,
                    column_types,
                    internal_columns,
                    internal_types,
                    rows: QueryRows::Rows {
                        named: Vec::new(),
                        positional: None,
                    },
                });
            }
            consumer.begin(engine, &columns, &internal_schema)?;
            if let Err(error) = operator.open() {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "open row consumer input",
                ));
            }
            'consume: loop {
                let batch = match operator.next() {
                    Ok(batch) => batch,
                    Err(error) => {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "execute row consumer input",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                let uqa_execution::Batch { schema, rows } = batch;
                for row in rows {
                    let row = uqa_execution::OwnedPhysicalRow::new(schema.clone(), row);
                    match consumer.consume(engine, row) {
                        Ok(QueryConsumerControl::Continue) => {}
                        Ok(QueryConsumerControl::Stop) => break 'consume,
                        Ok(QueryConsumerControl::Rewind) => {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                uqa_execution::ExecError::Other(
                                    "forward-only row consumer requested rewind".into(),
                                ),
                                "consume query row",
                            ));
                        }
                        Err(error) => {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                uqa_execution::ExecError::SQL(error),
                                "consume query row",
                            ));
                        }
                    }
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::Rows {
                named: Vec::new(),
                positional: None,
            }
        }
    };
    Ok(QueryOutput {
        columns,
        column_types,
        internal_columns,
        internal_types,
        rows,
    })
}

fn collect_directional_query_operator(
    engine: &Engine,
    columns: &[String],
    operator: &mut Box<dyn uqa_execution::PhysicalOperator + '_>,
    consumer: &Rc<dyn super::QueryRowConsumer>,
) -> Result<(), SQLError> {
    let support = operator.backward_scan_support();
    consumer.directional_scan_prepared(engine, support)?;
    if support != uqa_execution::BackwardScanSupport::Native {
        let placeholder: Box<dyn uqa_execution::PhysicalOperator> = Box::new(
            uqa_execution::TableScan::from_physical_rows(operator.row_schema().clone(), Vec::new()),
        );
        let inner = std::mem::replace(operator, placeholder);
        *operator = Box::new(uqa_execution::ScrollMaterialize::new(inner));
    }
    consumer.begin(engine, columns, operator.row_schema())?;
    if let Err(error) = operator.open() {
        return Err(close_after_physical_failure(
            operator.as_mut(),
            error,
            "open directional row consumer input",
        ));
    }
    loop {
        let batch = match operator.next_direction(consumer.scan_direction()) {
            Ok(batch) => batch,
            Err(error) => {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "execute directional row consumer input",
                ));
            }
        };
        let control = if let Some(batch) = batch {
            if batch.rows.len() != 1 {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    uqa_execution::ExecError::Other(format!(
                        "directional query operator returned {} rows in one pull",
                        batch.rows.len()
                    )),
                    "execute directional row consumer input",
                ));
            }
            let uqa_execution::Batch { schema, mut rows } = batch;
            let row = uqa_execution::OwnedPhysicalRow::new(
                schema,
                rows.pop().expect("directional batch width checked"),
            );
            consumer.consume(engine, row)
        } else {
            consumer.direction_exhausted(engine)
        };
        let mut control = match control {
            Ok(control) => control,
            Err(error) => {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    uqa_execution::ExecError::SQL(error),
                    "consume directional query row",
                ));
            }
        };
        loop {
            match control {
                QueryConsumerControl::Continue => break,
                QueryConsumerControl::Stop => {
                    operator.close().map_err(physical_exec_error)?;
                    return Ok(());
                }
                QueryConsumerControl::Rewind => {
                    if let Err(error) = operator.rewind() {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "rewind directional query input",
                        ));
                    }
                    control = match consumer.rewound(engine) {
                        Ok(control) => control,
                        Err(error) => {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                uqa_execution::ExecError::SQL(error),
                                "acknowledge directional query rewind",
                            ));
                        }
                    };
                }
            }
        }
    }
}

/// Collect decorrelated EXISTS keys directly from the filtered input. Direct column expressions stay as borrowed physical values; non-trivial key expressions are evaluated into an inline buffer. In either case there is no projected `PhysicalRow` materialization between the input and hash set.
#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(in crate::sql) fn collect_exists_key_operator<'a>(
    columns: Vec<String>,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    projections: &[ProjectionPlan],
    evaluator: SharedExpressionEvaluator<'a>,
) -> Result<QueryOutput, SQLError> {
    let internal_columns = operator.schema().to_vec();
    let internal_types = operator.row_schema().column_types().to_vec();
    let column_types = projections
        .iter()
        .map(|projection| {
            uqa_execution::scalar_type(
                &projection.expr,
                operator.row_schema(),
                evaluator.parameters(),
            )
            .ok()
            .flatten()
        })
        .collect();
    let direct_columns = projections
        .iter()
        .map(|projection| DirectColumnKey::compile(&projection.expr))
        .collect::<Option<Vec<_>>>();
    let mut keys = uqa_execution::CanonicalRowHashSet::new();
    if let Err(error) = operator.open() {
        return Err(close_after_physical_failure(
            operator.as_mut(),
            error,
            "open EXISTS key input",
        ));
    }
    loop {
        let batch = match operator.next() {
            Ok(batch) => batch,
            Err(error) => {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "collect EXISTS key input",
                ));
            }
        };
        let Some(batch) = batch else {
            break;
        };
        for row in &batch.rows {
            let view = batch.schema.view(row);
            let inserted = if let Some(direct_columns) = direct_columns.as_ref() {
                let mut key = SmallVec::<[&Value; 4]>::with_capacity(direct_columns.len());
                let mut contains_null = false;
                for column in direct_columns {
                    let Some(value) = column.value(&view) else {
                        contains_null = true;
                        break;
                    };
                    if matches!(value, Value::Null) {
                        contains_null = true;
                        break;
                    }
                    key.push(value);
                }
                if contains_null {
                    Ok(false)
                } else {
                    keys.insert_borrowed(&key)
                }
            } else {
                let mut key = SmallVec::<[Value; 4]>::with_capacity(projections.len());
                let mut contains_null = false;
                for projection in projections {
                    let value =
                        match evaluator.evaluate_physical(&projection.expr, &batch.schema, row) {
                            Ok(value) => value,
                            Err(error) => {
                                return Err(close_after_physical_failure(
                                    operator.as_mut(),
                                    error,
                                    "evaluate EXISTS key",
                                ));
                            }
                        };
                    if matches!(value, Value::Null) {
                        contains_null = true;
                        break;
                    }
                    key.push(value);
                }
                if contains_null {
                    Ok(false)
                } else {
                    keys.insert_values(&key)
                }
            };
            if let Err(error) = inserted {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "hash EXISTS key",
                ));
            }
        }
    }
    operator.close().map_err(physical_exec_error)?;
    Ok(QueryOutput {
        internal_columns,
        internal_types,
        column_types,
        columns,
        rows: QueryRows::ExistsKeySet(keys),
    })
}
