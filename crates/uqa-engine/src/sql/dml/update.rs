//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE execution, point-update fast paths, and patch eligibility.

use super::{
    apply_validated_prepared_document_rewrite, build_returning_row, coerce_to_column_type,
    dml_returning_result, dml_storage_error, dml_target_row, eval_mutation_assignment,
    eval_mutation_expr, finalize_partition_rewrite, index_vectors_for_type, lock_mutation_target,
    lock_physical_mutation_target, prepare_document_rewrite, referrers_to_for_actions,
    run_update_from, stage_prepared_document_rewrite, update_lock_strength,
    validate_dml_expression_qualifiers, validate_mutation_columns,
    validate_returning_alias_relations, BTreeMap, BTreeSet, BinaryOp, ColumnType, CteScope,
    DmlCommandMutationOverlay, DmlReturningShape, Engine, MutationAssignmentTarget,
    MutationLockTarget, PartitionRewritePolicy, PhysicalMutationLockTarget, ReturningProjectionRow,
    ReturningRowImage, ReturningRowImages, RowIndependentUpdateValues, SQLError, SQLParam,
    SQLResult, ScalarExpr, UpdatePlan, Value,
};

pub(in crate::sql) fn run_update(
    engine: &Engine,
    stmt: UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if engine.transaction_depth() != 0 {
        run_update_inner(engine, &stmt, params)
    } else {
        engine.transaction(move |engine| run_update_inner(engine, &stmt, params))
    }
}

pub(in crate::sql) fn run_update_inner(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    validate_mutation_columns(
        engine,
        &stmt.table,
        stmt.assignments
            .iter()
            .map(|assignment| assignment.column.as_str()),
        "UPDATE",
    )?;
    let mut ctes = CteScope::new();
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let assigned_columns = stmt
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &stmt.table,
        uqa_sql::ast::TriggerTiming::Before,
        uqa_sql::ast::TriggerEvent::Update,
        &assigned_columns,
    )?;

    if stmt.source.is_none() {
        let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
        if let Some(predicate) = stmt.predicate.as_ref() {
            validate_dml_expression_qualifiers(predicate, &allowed)?;
        }
        for assignment in &stmt.assignments {
            validate_dml_expression_qualifiers(&assignment.value, &allowed)?;
        }
    }

    // UPDATE ... FROM other [WHERE ...]: build the joined relation,
    // evaluate WHERE against each joined row, and apply assignments to the
    // matching target rows.
    if let Some(source) = stmt.source.as_deref() {
        let result = run_update_from(engine, stmt, source, params, &mut ctes)?;
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &stmt.table,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?;
        return Ok(result);
    }
    let target_tables = engine.hierarchy_scan_tables(&stmt.table, stmt.include_descendants)?;
    let target_hierarchy = engine
        .try_table_hierarchy(&stmt.table)
        .map_err(|error| SQLError::Internal(format!("read UPDATE hierarchy: {error}")))?;
    let target_is_partitioned =
        target_hierarchy.partition_spec.is_some() || target_hierarchy.partition_bound.is_some();
    let has_runtime_scope = !ctes.rows.is_empty() || !ctes.scalar_subqueries.is_empty();
    if !has_runtime_scope
        && target_tables.len() == 1
        && !target_is_partitioned
        && !engine.has_row_triggers(&stmt.table, uqa_sql::ast::TriggerEvent::Update)?
    {
        if let Some(result) = try_run_point_update(engine, stmt, params)? {
            crate::sql::triggers::fire_statement_triggers(
                engine,
                &stmt.table,
                uqa_sql::ast::TriggerTiming::After,
                uqa_sql::ast::TriggerEvent::Update,
                &assigned_columns,
            )?;
            return Ok(result);
        }
    }
    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let cancel = engine.cancellation_token();
    // A non-volatile predicate can still use the accelerated candidate set. A VOLATILE predicate must stay in the row loop because PostgreSQL exposes each preceding logical rewrite before qualifying the next candidate.
    let predicate_is_volatile = stmt.predicate.as_ref().is_some_and(|predicate| {
        crate::sql::volatility::expr_contains_volatile_function(engine, predicate)
    });
    let preselected = !has_runtime_scope && stmt.predicate.is_some() && !predicate_is_volatile;
    let candidates: Vec<(String, uqa_core::DocId)> = if preselected {
        let filter = stmt.predicate.as_ref().ok_or_else(|| {
            SQLError::Internal("UPDATE preselection is missing its predicate".into())
        })?;
        let mut candidates = Vec::new();
        for table in &target_tables {
            candidates.extend(
                crate::sql::where_eval::collect_where_doc_ids(
                    engine,
                    table,
                    &stmt.target_qualifier,
                    filter,
                    params,
                    &ctes,
                )?
                .into_iter()
                .map(|doc_id| (table.clone(), doc_id)),
            );
        }
        candidates
    } else {
        let mut candidates = Vec::new();
        for table in &target_tables {
            candidates.extend(
                engine
                    .table_doc_ids(table)?
                    .into_iter()
                    .map(|doc_id| (table.clone(), doc_id)),
            );
        }
        candidates
    };
    let snapshot_ctes = ctes.returning_statement_snapshot_scope();
    let overlay = DmlCommandMutationOverlay::new(engine);
    let mut prepared_updates = Vec::new();
    let mut referential_actions = super::ReferentialActionContext::default();
    let mut locked_ids = BTreeSet::new();
    for (storage_table, doc_id) in candidates {
        cancel.check()?;
        let Some(candidate) = engine.get_document(&storage_table, doc_id)? else {
            continue;
        };
        let candidate_row = dml_target_row(
            engine,
            &stmt.table,
            &stmt.target_qualifier,
            doc_id,
            &candidate,
        )?;
        if !preselected {
            if let Some(filter) = stmt.predicate.as_ref() {
                if !uqa_sql::expr::truthy(&eval_mutation_expr(
                    engine,
                    &snapshot_ctes,
                    filter,
                    Some(&candidate_row),
                    params,
                )?) {
                    continue;
                }
            }
        }
        let target = lock_physical_mutation_target(
            engine,
            &storage_table,
            &stmt.target_qualifier,
            doc_id,
            update_lock_strength(engine, &storage_table, &assigned_columns),
        )?;
        let PhysicalMutationLockTarget::Present { identity, recheck } = target else {
            continue;
        };
        let storage_table = identity.table;
        let doc_id = identity.doc_id;
        if !locked_ids.insert((storage_table.clone(), doc_id)) {
            continue;
        }
        if recheck {
            engine.refresh_explicit_statement_snapshot()?;
        }
        let Some(mut doc) = engine.get_document(&storage_table, doc_id)? else {
            continue;
        };
        let original_doc = doc.clone();
        let target_row = dml_target_row(
            engine,
            &stmt.table,
            &stmt.target_qualifier,
            doc_id,
            &original_doc,
        )?;
        if recheck || preselected {
            if let Some(filter) = stmt.predicate.as_ref() {
                if !uqa_sql::expr::truthy(&eval_mutation_expr(
                    engine,
                    &snapshot_ctes,
                    filter,
                    Some(&target_row),
                    params,
                )?) {
                    continue;
                }
            }
        }
        for assignment in &stmt.assignments {
            let value = eval_mutation_assignment(
                engine,
                &snapshot_ctes,
                MutationAssignmentTarget {
                    table: &stmt.table,
                    column: &assignment.column,
                    action: "UPDATE",
                },
                &assignment.value,
                Some(&target_row),
                params,
            )?;
            if let Some(value) = value {
                doc.insert(assignment.column.clone(), value);
            } else {
                doc.remove(&assignment.column);
            }
        }
        let Some(triggered_document) = crate::sql::triggers::fire_before_row_triggers(
            engine,
            &storage_table,
            uqa_sql::ast::TriggerEvent::Update,
            doc_id,
            Some(&original_doc),
            Some(&doc),
            &assigned_columns,
        )?
        else {
            continue;
        };
        doc = triggered_document;
        if let Some(mut prepared) = prepare_document_rewrite(
            engine,
            &storage_table,
            doc_id,
            original_doc,
            doc,
            params,
            &mut referential_actions,
        )? {
            finalize_partition_rewrite(
                engine,
                &mut prepared,
                &stmt.table,
                params,
                stmt.include_descendants,
                PartitionRewritePolicy::Move,
            )?;
            let mut after_row_events = Vec::new();
            let rewritten_doc_id = stage_prepared_document_rewrite(
                engine,
                &mut prepared,
                params,
                Some(&assigned_columns),
                &mut after_row_events,
            )?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    ReturningProjectionRow {
                        table: &stmt.table,
                        target_qualifier: &stmt.target_qualifier,
                        images: ReturningRowImages {
                            old: Some(ReturningRowImage {
                                doc_id: prepared.doc_id,
                                document: &prepared.old_document,
                            }),
                            new: Some(ReturningRowImage {
                                doc_id: rewritten_doc_id,
                                document: &prepared.new_document,
                            }),
                        },
                        aliases: &stmt.returning_aliases,
                        context: None,
                    },
                    &stmt.returning,
                    params,
                    &snapshot_ctes,
                )?);
            }
            affected += 1;
            prepared_updates.push((prepared, after_row_events));
        }
    }
    drop(overlay);
    if !prepared_updates.is_empty() {
        engine.prepare_explicit_transaction_writer()?;
        for (prepared, _) in &mut prepared_updates {
            apply_validated_prepared_document_rewrite(engine, prepared)?;
        }
    }
    for (_, events) in prepared_updates {
        crate::sql::triggers::fire_after_row_trigger_events(engine, &events)?;
    }
    referential_actions.fire_after_statement_triggers(engine)?;
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &stmt.table,
        uqa_sql::ast::TriggerTiming::After,
        uqa_sql::ast::TriggerEvent::Update,
        &assigned_columns,
    )?;
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            engine,
            DmlReturningShape {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: None,
            },
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

pub(in crate::sql) fn try_run_point_update(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<SQLResult>, SQLError> {
    if engine
        .try_describe_table(&stmt.table)
        .map_err(|error| dml_storage_error("UPDATE", error))?
        .is_some_and(|columns| columns.iter().any(|column| column.generated.is_some()))
    {
        return Ok(None);
    }
    if !stmt.returning.is_empty() {
        return Ok(None);
    }
    let Some((lookup_field, lookup_value)) =
        point_lookup_filter(stmt.predicate.as_ref(), engine, params)?
    else {
        return Ok(None);
    };
    let Some((updates, vectors)) = row_independent_update_values(engine, stmt, params)? else {
        return Ok(None);
    };
    if !can_patch_update_without_full_row(engine, &stmt.table, &updates)? {
        return Ok(None);
    }
    if matches!(lookup_value, Value::Null) {
        return Ok(Some(SQLResult::from_affected(0)));
    }
    if !point_lookup_field_is_unique(engine, &stmt.table, &lookup_field)? {
        return Ok(None);
    }
    let Some(doc_id) = engine.find_doc_id_by_field(&stmt.table, &lookup_field, &lookup_value)?
    else {
        return Ok(Some(SQLResult::from_affected(0)));
    };
    let target = lock_mutation_target(
        engine,
        &stmt.table,
        &stmt.target_qualifier,
        doc_id,
        update_lock_strength(
            engine,
            &stmt.table,
            &stmt
                .assignments
                .iter()
                .map(|assignment| assignment.column.clone())
                .collect::<Vec<_>>(),
        ),
    )?;
    let MutationLockTarget::Present { doc_id, .. } = target else {
        return Ok(Some(SQLResult::from_affected(0)));
    };
    engine.prepare_explicit_transaction_writer()?;
    if engine.find_doc_id_by_field(&stmt.table, &lookup_field, &lookup_value)? != Some(doc_id) {
        return Ok(Some(SQLResult::from_affected(0)));
    }
    let affected =
        engine.patch_document_fields_with_vector_values(&stmt.table, doc_id, &updates, &vectors)?;
    Ok(Some(SQLResult::from_affected(u64::from(affected))))
}

pub(in crate::sql) fn point_lookup_filter(
    filter: Option<&ScalarExpr>,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Option<(String, Value)>, SQLError> {
    let Some(ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    }) = filter
    else {
        return Ok(None);
    };
    if let Some(field) = top_level_column(lhs) {
        if expr_is_row_independent(rhs) {
            let ctes = CteScope::new();
            return Ok(Some((
                field.to_string(),
                eval_mutation_expr(engine, &ctes, rhs, None, params)?,
            )));
        }
    }
    if let Some(field) = top_level_column(rhs) {
        if expr_is_row_independent(lhs) {
            let ctes = CteScope::new();
            return Ok(Some((
                field.to_string(),
                eval_mutation_expr(engine, &ctes, lhs, None, params)?,
            )));
        }
    }
    Ok(None)
}

pub(in crate::sql) fn top_level_column(expr: &ScalarExpr) -> Option<&str> {
    match expr {
        ScalarExpr::Column(name) => Some(name),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column),
        _ => None,
    }
}

pub(in crate::sql) fn row_independent_update_values(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<RowIndependentUpdateValues>, SQLError> {
    let mut updates = BTreeMap::new();
    let mut vectors = BTreeMap::new();
    let ctes = CteScope::new();
    for assignment in &stmt.assignments {
        if !expr_is_row_independent(&assignment.value) {
            return Ok(None);
        }
        let value = coerce_to_column_type(
            engine,
            &stmt.table,
            &assignment.column,
            eval_mutation_expr(engine, &ctes, &assignment.value, None, params)?,
        )?;
        if let Some(ty @ (ColumnType::Vector(_) | ColumnType::Tensor(_))) = engine
            .column_type(&stmt.table, &assignment.column)
            .map_err(|err| dml_storage_error("UPDATE", err))?
        {
            let values = index_vectors_for_type(&value, &ty)?;
            vectors.insert(assignment.column.clone(), values);
        }
        updates.insert(assignment.column.clone(), value);
    }
    Ok(Some((updates, vectors)))
}

pub(in crate::sql) fn expr_is_row_independent(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => true,
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(expr_is_row_independent),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_is_row_independent(lhs) && expr_is_row_independent(rhs)
        }
        ScalarExpr::Not(inner) | ScalarExpr::UnaryMinus(inner) => expr_is_row_independent(inner),
        ScalarExpr::IsNull { expr, .. } => expr_is_row_independent(expr),
        ScalarExpr::Between { expr, low, high } => {
            expr_is_row_independent(expr)
                && expr_is_row_independent(low)
                && expr_is_row_independent(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_is_row_independent(expr) && list.iter().all(expr_is_row_independent)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().map_or(true, expr_is_row_independent)
                && when.iter().all(|(condition, result)| {
                    expr_is_row_independent(condition) && expr_is_row_independent(result)
                })
                && else_branch.as_deref().map_or(true, expr_is_row_independent)
        }
        ScalarExpr::Cast { expr, .. } => expr_is_row_independent(expr),
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Func { .. }
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}

pub(in crate::sql) fn can_patch_update_without_full_row(
    engine: &Engine,
    table: &str,
    updates: &BTreeMap<String, Value>,
) -> Result<bool, SQLError> {
    if engine
        .try_check_constraint_definitions(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| constraint.enforced)
    {
        return Ok(false);
    }
    let update_keys: BTreeSet<&str> = updates.keys().map(String::as_str).collect();
    if engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .iter()
        .any(|col| {
            col.not_null
                && col.auto_increment.is_none()
                && matches!(updates.get(&col.name), Some(Value::Null))
        })
    {
        return Ok(false);
    }
    if engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| {
            constraint
                .columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    if engine
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .filter(|fk| fk.enforced)
        .any(|fk| {
            fk.local_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    if referrers_to_for_actions(engine, table)?
        .iter()
        .any(|(_, fk)| {
            fk.ref_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    Ok(true)
}

pub(in crate::sql) fn point_lookup_field_is_unique(
    engine: &Engine,
    table: &str,
    lookup_field: &str,
) -> Result<bool, SQLError> {
    Ok(engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| constraint.columns.len() == 1 && constraint.columns[0] == lookup_field))
}
