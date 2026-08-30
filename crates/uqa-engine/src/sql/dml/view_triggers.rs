//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-oriented DML execution for views with `INSTEAD OF` triggers.

use super::{
    build_join_spill_with_ctes, build_returning_value_row, dml_join_rows, dml_returning_result,
    eval_mutation_expr, validate_dml_expression_qualifiers, validate_returning_alias_relations,
    BTreeSet, ColumnType, CteScope, DeletePlan, DmlReturningShape, Document, Engine, InsertPlan,
    OwnedPhysicalRow, PhysicalRow, ReturningValueProjectionRow, RowSchema, SQLError, SQLParam,
    SQLResult, ScalarExpr, UpdatePlan, Value,
};

struct ViewDmlTarget {
    canonical_name: String,
    definition: crate::StoredView,
    columns: Vec<String>,
    types: Vec<Option<ColumnType>>,
}

pub(super) fn target_view_kind(
    engine: &Engine,
    name: &str,
) -> Result<Option<crate::StoredViewKind>, SQLError> {
    let candidates = engine
        .relation_lookup_candidates(name)
        .map_err(|error| SQLError::Internal(format!("resolve DML relation `{name}`: {error}")))?;
    let tables = engine.storage.tables.read();
    let views = engine.durable.views.read();
    for relation in candidates {
        if tables.contains_key(&relation) {
            return Ok(None);
        }
        if let Some(view) = views.get(&relation) {
            return Ok(Some(view.kind));
        }
    }
    Ok(None)
}

pub(super) fn target_is_view(engine: &Engine, name: &str) -> Result<bool, SQLError> {
    Ok(target_view_kind(engine, name)?.is_some())
}

fn resolve_view_target(engine: &Engine, name: &str) -> Result<ViewDmlTarget, SQLError> {
    let canonical_name = engine
        .try_resolve_view_name(name)
        .map_err(|error| SQLError::Internal(format!("resolve DML view `{name}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    let definition = engine
        .view_definition(&canonical_name)?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    if definition.kind != crate::StoredViewKind::View {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("relation \"{canonical_name}\" is not a view"),
        });
    }
    let schema = engine.stored_view_schema(&definition)?;
    let columns = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect::<Vec<_>>();
    let types = (0..columns.len())
        .map(|position| schema.column_type(position).cloned())
        .collect();
    Ok(ViewDmlTarget {
        canonical_name,
        definition,
        columns,
        types,
    })
}

fn materialize_view_rows(
    engine: &Engine,
    target: &ViewDmlTarget,
    required_columns: Option<&BTreeSet<String>>,
    params: &[SQLParam],
    scope: &mut CteScope,
) -> Result<Vec<Vec<Value>>, SQLError> {
    let mut query = target.definition.query.clone();
    if let Some(required_columns) = required_columns {
        let required_positions = target
            .columns
            .iter()
            .enumerate()
            .filter_map(|(position, column)| required_columns.contains(column).then_some(position))
            .collect::<BTreeSet<_>>();
        super::prune_unused_query_outputs(&mut query, &required_positions, target.columns.len());
    }
    let result = crate::sql::select::execute_query_plan_with_ctes(engine, &query, params, scope)?;
    if result.columns.len() != target.columns.len() {
        return Err(SQLError::Internal(format!(
            "view `{}` returned {} columns for a {}-column row type",
            target.canonical_name,
            result.columns.len(),
            target.columns.len()
        )));
    }
    (0..result.rows.len())
        .map(|row| {
            (0..target.columns.len())
                .map(|column| {
                    result.value_at(row, column).cloned().ok_or_else(|| {
                        SQLError::Internal(format!(
                            "view `{}` omitted result column {}",
                            target.canonical_name,
                            column + 1
                        ))
                    })
                })
                .collect()
        })
        .collect()
}

fn collect_view_expression_columns(
    expression: &ScalarExpr,
    columns: &mut BTreeSet<String>,
) -> bool {
    expression.collect_columns(columns)
}

fn required_view_update_columns(
    engine: &Engine,
    target: &ViewDmlTarget,
    stmt: &UpdatePlan,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    let Some(mut columns) = crate::sql::rules::relation_rule_row_columns(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Update,
    )?
    else {
        return Ok(None);
    };
    columns.extend(
        stmt.assignments
            .iter()
            .map(|assignment| assignment.column.clone()),
    );
    for assignment in &stmt.assignments {
        if !collect_view_expression_columns(&assignment.value, &mut columns) {
            return Ok(None);
        }
    }
    if let Some(predicate) = stmt.predicate.as_ref() {
        if !collect_view_expression_columns(predicate, &mut columns) {
            return Ok(None);
        }
    }
    Ok(Some(columns))
}

fn required_view_delete_columns(
    engine: &Engine,
    target: &ViewDmlTarget,
    stmt: &DeletePlan,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    let Some(mut columns) = crate::sql::rules::relation_rule_row_columns(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Delete,
    )?
    else {
        return Ok(None);
    };
    if let Some(predicate) = stmt.predicate.as_ref() {
        if !collect_view_expression_columns(predicate, &mut columns) {
            return Ok(None);
        }
    }
    Ok(Some(columns))
}

fn target_row(
    target: &ViewDmlTarget,
    qualifier: &str,
    values: &[Value],
) -> Result<OwnedPhysicalRow, SQLError> {
    if values.len() != target.columns.len() {
        return Err(SQLError::Internal(
            "view DML row does not match its declared row type".into(),
        ));
    }
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, target.columns.clone(), target.types.clone()),
        PhysicalRow::from_values(values.to_vec()),
    ))
}

fn coerce_view_value(
    target: &ViewDmlTarget,
    position: usize,
    value: Value,
) -> Result<Value, SQLError> {
    match target.types[position].as_ref() {
        Some(ty) => crate::sql::convert_value_to_column_type(value, ty),
        None => Ok(value),
    }
}

fn target_columns(
    target: &ViewDmlTarget,
    explicit: &[String],
    operation: &str,
) -> Result<Vec<String>, SQLError> {
    let columns = if explicit.is_empty() {
        target.columns.clone()
    } else {
        explicit.to_vec()
    };
    let mut seen = BTreeSet::new();
    for column in &columns {
        if !seen.insert(column) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{column}\" specified more than once"),
            });
        }
        if !target.columns.contains(column) {
            return Err(SQLError::UnknownColumn(format!(
                "{}.{column}",
                target.canonical_name
            )));
        }
    }
    if columns.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{operation} against a zero-column view is not supported"
        )));
    }
    Ok(columns)
}

fn values_from_result(result: SQLResult) -> Result<Vec<Vec<Value>>, SQLError> {
    (0..result.rows.len())
        .map(|row| {
            (0..result.columns.len())
                .map(|column| {
                    result.value_at(row, column).cloned().ok_or_else(|| {
                        SQLError::Internal(format!(
                            "query result omitted output column {}",
                            column + 1
                        ))
                    })
                })
                .collect()
        })
        .collect()
}

fn view_document(target: &ViewDmlTarget, values: &[Value]) -> Result<Document, SQLError> {
    if values.len() != target.columns.len() {
        return Err(SQLError::Internal(
            "view rule row does not match its declared row type".into(),
        ));
    }
    Ok(target
        .columns
        .iter()
        .cloned()
        .zip(values.iter().cloned())
        .collect())
}

fn cached_view_document(
    target: &ViewDmlTarget,
    values: &[Option<Value>],
) -> Result<Document, SQLError> {
    if values.len() != target.columns.len() {
        return Err(SQLError::Internal(
            "cached view rule row does not match its declared row type".into(),
        ));
    }
    Ok(target
        .columns
        .iter()
        .cloned()
        .zip(values)
        .filter_map(|(column, value)| value.clone().map(|value| (column, value)))
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn evaluate_insert_rule_column(
    engine: &Engine,
    target: &ViewDmlTarget,
    positions: &[usize],
    expressions: &[ScalarExpr],
    column: &str,
    values: &mut [Option<Value>],
    params: &[SQLParam],
    scope: &CteScope,
) -> Result<Value, SQLError> {
    let target_position = target
        .columns
        .iter()
        .position(|candidate| candidate == column)
        .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?;
    if let Some(value) = values[target_position].as_ref() {
        return Ok(value.clone());
    }
    let value = if let Some(input_position) = positions
        .iter()
        .position(|position| *position == target_position)
    {
        let expression = expressions.get(input_position).ok_or_else(|| {
            SQLError::Internal("view rule INSERT input lost its expression".into())
        })?;
        if matches!(expression, ScalarExpr::Default) {
            Value::Null
        } else {
            eval_mutation_expr(engine, scope, expression, None, params)?
        }
    } else {
        Value::Null
    };
    let value = coerce_view_value(target, target_position, value)?;
    values[target_position] = Some(value.clone());
    Ok(value)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_insert_rule_columns(
    engine: &Engine,
    target: &ViewDmlTarget,
    positions: &[usize],
    expressions: &[ScalarExpr],
    required: &BTreeSet<String>,
    values: &mut [Option<Value>],
    params: &[SQLParam],
    scope: &CteScope,
) -> Result<(), SQLError> {
    for column in required {
        let _ = evaluate_insert_rule_column(
            engine,
            target,
            positions,
            expressions,
            column,
            values,
            params,
            scope,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_suppressed_view_insert_rules(
    engine: &Engine,
    read_engine: &Engine,
    stmt: &InsertPlan,
    target: &ViewDmlTarget,
    positions: &[usize],
    columns: &[String],
    implicit_columns: bool,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    let snapshot = ctes.returning_statement_snapshot_scope();
    let mut cached_rows = Vec::with_capacity(stmt.rows.len());
    for expressions in &stmt.rows {
        if expressions.len() > columns.len()
            || (!implicit_columns && expressions.len() != columns.len())
        {
            return Err(SQLError::TypeMismatch(format!(
                "row width {} != column count {}",
                expressions.len(),
                columns.len()
            )));
        }
        let values = vec![None; target.columns.len()];
        cached_rows.push(values);
    }
    let rule_rows = cached_rows
        .iter()
        .map(|values| {
            Ok(crate::sql::rules::RuleRowImage {
                old_storage_table: None,
                old_doc_id: None,
                old: None,
                new_storage_table: None,
                new_doc_id: None,
                new: Some(cached_view_document(target, values)?),
                context: None,
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut rule_batch = crate::sql::rules::prepare_rule_batch_with_projection(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Insert,
        rule_rows,
        |row_index, side, column| {
            if matches!(side, crate::sql::rules::RuleRowSide::Old) {
                return Ok(None);
            }
            let expressions = stmt
                .rows
                .get(row_index)
                .ok_or_else(|| SQLError::Internal("view rule INSERT lost its input row".into()))?;
            let values = cached_rows
                .get_mut(row_index)
                .ok_or_else(|| SQLError::Internal("view rule INSERT lost its cached row".into()))?;
            evaluate_insert_rule_column(
                read_engine,
                target,
                positions,
                expressions,
                column,
                values,
                params,
                &snapshot,
            )
            .map(Some)
        },
    )?;
    let action_columns = rule_batch.missing_action_row_columns();
    for ((expressions, values), (_, required)) in
        stmt.rows.iter().zip(&mut cached_rows).zip(&action_columns)
    {
        evaluate_insert_rule_columns(
            read_engine,
            target,
            positions,
            expressions,
            required,
            values,
            params,
            &snapshot,
        )?;
    }
    rule_batch.supplement_rows(
        cached_rows
            .iter()
            .map(|values| {
                Ok(crate::sql::rules::RuleRowImage {
                    old_storage_table: None,
                    old_doc_id: None,
                    old: None,
                    new_storage_table: None,
                    new_doc_id: None,
                    new: Some(cached_view_document(target, values)?),
                    context: None,
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?,
    )?;
    let outcome = rule_batch.execute_actions_with_affected(
        engine,
        crate::sql::rules::RuleReturningRequest::from_plan(
            &stmt.returning,
            &stmt.returning_aliases,
            &stmt.subqueries,
        ),
    )?;
    if let Some(returning) = outcome.returning {
        return returning.project(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes,
                supplemental_schema: None,
            },
        );
    }
    finish_view_dml(
        engine,
        DmlReturningShape {
            table: &target.canonical_name,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes,
            supplemental_schema: None,
        },
        Vec::new(),
        outcome.affected_rows,
    )
}

fn finish_view_dml(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    returning_rows: Vec<OwnedPhysicalRow>,
    affected: u64,
) -> Result<SQLResult, SQLError> {
    if shape.returning.is_empty() {
        return Ok(SQLResult::from_affected(affected));
    }
    dml_returning_result(engine, shape, returning_rows, affected)
}

pub(super) fn run_view_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(engine, &stmt.table)?;
    if stmt.on_conflict.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "INSERT with ON CONFLICT clause cannot be used with a view".into(),
        });
    }
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    let columns = target_columns(&target, &stmt.columns, "INSERT")?;
    let implicit_columns = stmt.columns.is_empty();
    let positions = columns
        .iter()
        .map(|column| {
            target
                .columns
                .iter()
                .position(|candidate| candidate == column)
                .ok_or_else(|| SQLError::UnknownColumn(column.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let original_query_survives = !crate::sql::rules::relation_suppresses_original_query(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Insert,
    )?;
    let has_before_statement_trigger = original_query_survives
        && !engine
            .triggers_for(
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Insert,
                false,
                &[],
            )?
            .is_empty();
    let statement_snapshot = has_before_statement_trigger
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
        )?;
    }
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine();
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let suppressed_source_is_unused = stmt.source.is_some()
        && !crate::sql::rules::relation_rules_require_event_rows(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Insert,
        )?;
    if stmt.view_rule_relations.is_empty()
        && !original_query_survives
        && (stmt.source.is_none() || suppressed_source_is_unused)
    {
        return run_suppressed_view_insert_rules(
            engine,
            read_engine,
            stmt,
            &target,
            &positions,
            &columns,
            implicit_columns,
            params,
            &ctes,
        );
    }
    let input_rows = if let Some(source) = stmt.source.as_deref() {
        let mut source = source.clone();
        if !original_query_survives {
            if let Some(required_columns) = crate::sql::rules::relation_rule_row_columns(
                engine,
                &target.canonical_name,
                uqa_sql::ast::RuleEvent::Insert,
            )? {
                let required_positions = columns
                    .iter()
                    .enumerate()
                    .filter_map(|(position, column)| {
                        required_columns.contains(column).then_some(position)
                    })
                    .collect::<BTreeSet<_>>();
                super::prune_unused_query_outputs(&mut source, &required_positions, columns.len());
            }
        }
        let mut source_scope = ctes.returning_statement_snapshot_scope();
        values_from_result(crate::sql::select::execute_query_plan_with_ctes(
            read_engine,
            &source,
            params,
            &mut source_scope,
        )?)?
    } else {
        let snapshot = ctes.returning_statement_snapshot_scope();
        stmt.rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|expression| {
                        if matches!(expression, ScalarExpr::Default) {
                            Ok(Value::Null)
                        } else {
                            eval_mutation_expr(read_engine, &snapshot, expression, None, params)
                        }
                    })
                    .collect()
            })
            .collect::<Result<Vec<Vec<_>>, SQLError>>()?
    };
    let mut proposed_rows = Vec::with_capacity(input_rows.len());
    for input in input_rows {
        if input.len() > columns.len() || (!implicit_columns && input.len() != columns.len()) {
            return Err(SQLError::TypeMismatch(format!(
                "row width {} != column count {}",
                input.len(),
                columns.len()
            )));
        }
        let mut new = vec![Value::Null; target.columns.len()];
        for (input_position, target_position) in positions.iter().copied().enumerate() {
            if let Some(value) = input.get(input_position) {
                new[target_position] = coerce_view_value(&target, target_position, value.clone())?;
            }
        }
        proposed_rows.push(new);
    }
    let rule_rows = proposed_rows
        .iter()
        .map(|new| {
            Ok(crate::sql::rules::RuleRowImage {
                old_storage_table: None,
                old_doc_id: None,
                old: None,
                new_storage_table: None,
                new_doc_id: None,
                new: Some(view_document(&target, new)?),
                context: None,
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let outer_rule_batches = super::prepare_view_rule_batches(super::ViewRuleBatchRequest {
        engine,
        relations: &stmt.view_rule_relations,
        event: uqa_sql::ast::RuleEvent::Insert,
        rows: &rule_rows,
        params,
        scope: &ctes,
        insert_plans: &stmt.view_rule_insert_plans,
        update_plans: &[],
        document_relation: Some(&target.canonical_name),
    })?;
    let rule_batch = crate::sql::rules::prepare_rule_batch(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Insert,
        rule_rows,
    )?;
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();
    for (index, new) in proposed_rows.into_iter().enumerate() {
        if rule_batch.suppresses(index) {
            continue;
        }
        let Some(final_new) = crate::sql::triggers::fire_instead_of_row_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerEvent::Insert,
            None,
            Some(&new),
            &[],
        )?
        else {
            continue;
        };
        affected += 1;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_value_row(
                engine,
                ReturningValueProjectionRow {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    current: &final_new,
                    old: None,
                    new: Some(&final_new),
                    aliases: &stmt.returning_aliases,
                    context: None,
                },
                &stmt.returning,
                params,
                &ctes,
            )?);
        }
    }
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
        )?;
    }
    let mut result = finish_view_dml(
        engine,
        DmlReturningShape {
            table: &target.canonical_name,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes: &ctes,
            supplemental_schema: None,
        },
        returning_rows,
        affected,
    )?;
    let rule_outcome = rule_batch.execute_actions_with_affected(
        engine,
        crate::sql::rules::RuleReturningRequest::from_plan(
            &stmt.returning,
            &stmt.returning_aliases,
            &stmt.subqueries,
        ),
    )?;
    let outer_rule_outcome = outer_rule_batches
        .execute_actions_with_affected(engine, stmt.view_rule_returning.as_ref())?;
    if rule_outcome.returning.is_some() && outer_rule_outcome.returning.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    if let Some(rule_returning) = rule_outcome.returning {
        return rule_returning.project(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: None,
            },
        );
    }
    if let Some(outer_returning) = outer_rule_outcome.returning {
        return outer_returning.project(engine, params, &ctes, None);
    }
    if !original_query_survives && rule_outcome.executed_action {
        result.affected_rows = rule_outcome.affected_rows;
    }
    if !original_query_survives && outer_rule_outcome.executed_action {
        result.affected_rows = outer_rule_outcome.affected_rows;
    }
    Ok(result)
}

enum ViewDmlSourceMatch {
    TargetOnly,
    Source(OwnedPhysicalRow),
}

struct PendingViewUpdate {
    old: Vec<Value>,
    new: Vec<Value>,
    source_context: Option<OwnedPhysicalRow>,
    evaluation_row: OwnedPhysicalRow,
    evaluated_assignments: BTreeSet<String>,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_view_update_assignments(
    engine: &Engine,
    target: &ViewDmlTarget,
    stmt: &UpdatePlan,
    required: Option<&BTreeSet<String>>,
    pending: &mut PendingViewUpdate,
    params: &[SQLParam],
    scope: &CteScope,
) -> Result<(), SQLError> {
    for assignment in &stmt.assignments {
        if pending.evaluated_assignments.contains(&assignment.column)
            || required.is_some_and(|required| !required.contains(&assignment.column))
        {
            continue;
        }
        let position = target
            .columns
            .iter()
            .position(|column| column == &assignment.column)
            .ok_or_else(|| SQLError::UnknownColumn(assignment.column.clone()))?;
        let value = if matches!(assignment.value, ScalarExpr::Default) {
            Value::Null
        } else {
            eval_mutation_expr(
                engine,
                scope,
                &assignment.value,
                Some(&pending.evaluation_row),
                params,
            )?
        };
        pending.new[position] = coerce_view_value(target, position, value)?;
        pending
            .evaluated_assignments
            .insert(assignment.column.clone());
    }
    Ok(())
}

fn matching_source_context(
    engine: &Engine,
    target_row: &OwnedPhysicalRow,
    source_rows: Option<&uqa_execution::SharedSpill>,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<ViewDmlSourceMatch>, SQLError> {
    let Some(source_rows) = source_rows else {
        let qualifies = predicate.map_or(Ok(true), |predicate| {
            eval_mutation_expr(engine, ctes, predicate, Some(target_row), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        return Ok(qualifies.then_some(ViewDmlSourceMatch::TargetOnly));
    };
    for source in source_rows
        .read_rows()
        .map_err(crate::sql::select::physical_exec_error)?
    {
        let source = source.map_err(crate::sql::select::physical_exec_error)?;
        let joined = dml_join_rows(target_row, &source);
        let qualifies = predicate.map_or(Ok(true), |predicate| {
            eval_mutation_expr(engine, ctes, predicate, Some(&joined), params)
                .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        if qualifies {
            return Ok(Some(ViewDmlSourceMatch::Source(source)));
        }
    }
    Ok(None)
}

fn view_qualification_references_target(
    target: &ViewDmlTarget,
    target_qualifier: &str,
    predicate: Option<&ScalarExpr>,
) -> bool {
    let Some(predicate) = predicate else {
        return false;
    };
    if crate::sql::select::expr_contains_subquery(predicate) {
        return true;
    }
    if crate::sql::select::expr_qualifiers(predicate)
        .iter()
        .any(|qualifier| {
            qualifier.eq_ignore_ascii_case(target_qualifier)
                || qualifier.eq_ignore_ascii_case(&target.canonical_name)
        })
    {
        return true;
    }
    if !crate::sql::select::expr_has_unqualified_column(predicate) {
        return false;
    }
    let mut columns = BTreeSet::new();
    !predicate.collect_columns(&mut columns)
        || columns.iter().any(|column| target.columns.contains(column))
}

struct ViewSourceQualification<'a> {
    engine: &'a Engine,
    target: &'a ViewDmlTarget,
    target_qualifier: &'a str,
    predicate: Option<&'a ScalarExpr>,
    candidates: &'a [Vec<Value>],
    source_rows: &'a uqa_execution::SharedSpill,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
}

fn count_view_source_qualifications(
    context: ViewSourceQualification<'_>,
) -> Result<usize, SQLError> {
    let ViewSourceQualification {
        engine,
        target,
        target_qualifier,
        predicate,
        candidates,
        source_rows,
        params,
        ctes,
    } = context;
    let references_target =
        view_qualification_references_target(target, target_qualifier, predicate);
    let mut count = 0;
    if !references_target {
        for source in source_rows
            .read_rows()
            .map_err(crate::sql::select::physical_exec_error)?
        {
            let source = source.map_err(crate::sql::select::physical_exec_error)?;
            let qualifies = predicate.map_or(Ok(true), |predicate| {
                eval_mutation_expr(engine, ctes, predicate, Some(&source), params)
                    .map(|value| uqa_sql::expr::truthy(&value))
            })?;
            count += usize::from(qualifies);
        }
        return Ok(count);
    }
    for candidate in candidates {
        let physical = target_row(target, target_qualifier, candidate)?;
        for source in source_rows
            .read_rows()
            .map_err(crate::sql::select::physical_exec_error)?
        {
            let source = source.map_err(crate::sql::select::physical_exec_error)?;
            let joined = dml_join_rows(&physical, &source);
            let qualifies = predicate.map_or(Ok(true), |predicate| {
                eval_mutation_expr(engine, ctes, predicate, Some(&joined), params)
                    .map(|value| uqa_sql::expr::truthy(&value))
            })?;
            count += usize::from(qualifies);
        }
    }
    Ok(count)
}

pub(super) fn run_view_update_inner(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(engine, &stmt.table)?;
    let assigned_columns = stmt
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    let _ = target_columns(&target, &assigned_columns, "UPDATE")?;
    if stmt.source.is_none() {
        let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
        if let Some(predicate) = stmt.predicate.as_ref() {
            validate_dml_expression_qualifiers(predicate, &allowed)?;
        }
        for assignment in &stmt.assignments {
            validate_dml_expression_qualifiers(&assignment.value, &allowed)?;
        }
    }
    let original_query_survives = !crate::sql::rules::relation_suppresses_original_query(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Update,
    )?;
    let has_before_statement_trigger = original_query_survives
        && !engine
            .triggers_for(
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Update,
                false,
                &assigned_columns,
            )?
            .is_empty();
    let statement_snapshot = has_before_statement_trigger
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Update,
            &assigned_columns,
        )?;
    }
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine();
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let row_independent_update_qualification = if stmt.source.is_none()
        && !engine
            .rules_for(&target.canonical_name, uqa_sql::ast::RuleEvent::Update)?
            .is_empty()
    {
        super::row_independent_mutation_qualification_count(
            read_engine,
            stmt.predicate.as_ref(),
            params,
            &ctes,
        )?
    } else {
        None
    };
    if stmt.view_rule_relations.is_empty()
        && !original_query_survives
        && !crate::sql::rules::relation_rules_require_event_rows(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Update,
        )?
    {
        validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
        let rule_batch = crate::sql::rules::prepare_rule_batch(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Update,
            Vec::new(),
        )?;
        let outcome = rule_batch.execute_actions_with_affected(
            engine,
            crate::sql::rules::RuleReturningRequest::from_plan(
                &stmt.returning,
                &stmt.returning_aliases,
                &stmt.subqueries,
            ),
        )?;
        if let Some(returning) = outcome.returning {
            return returning.project(
                engine,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &ctes,
                    supplemental_schema: None,
                },
            );
        }
        return finish_view_dml(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: None,
            },
            Vec::new(),
            0,
        );
    }
    let mut source_scope = ctes.returning_statement_snapshot_scope();
    let source_rows = stmt
        .source
        .as_deref()
        .map(|source| build_join_spill_with_ctes(read_engine, source, params, &mut source_scope))
        .transpose()?;
    validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        source_rows
            .as_ref()
            .map(uqa_execution::SharedSpill::row_schema),
    )?;
    let mut target_scope = ctes.returning_statement_snapshot_scope();
    let required_columns = (!original_query_survives)
        .then(|| required_view_update_columns(engine, &target, stmt))
        .transpose()?
        .flatten();
    let candidates = materialize_view_rows(
        read_engine,
        &target,
        required_columns.as_ref(),
        params,
        &mut target_scope,
    )?;
    let snapshot = ctes.returning_statement_snapshot_scope();
    let source_update_qualification_count = source_rows
        .as_ref()
        .map(|source_rows| {
            count_view_source_qualifications(ViewSourceQualification {
                engine: read_engine,
                target: &target,
                target_qualifier: &stmt.target_qualifier,
                predicate: stmt.predicate.as_ref(),
                candidates: &candidates,
                source_rows,
                params,
                ctes: &snapshot,
            })
        })
        .transpose()?;
    let condition_columns = if !original_query_survives && stmt.view_rule_relations.is_empty() {
        Some(crate::sql::rules::relation_condition_row_columns(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Update,
        )?)
    } else {
        None
    };
    let mut pending = Vec::new();
    for old in candidates {
        let physical = target_row(&target, &stmt.target_qualifier, &old)?;
        let Some(source_match) = matching_source_context(
            read_engine,
            &physical,
            source_rows.as_ref(),
            stmt.predicate.as_ref(),
            params,
            &snapshot,
        )?
        else {
            continue;
        };
        let source_context = match source_match {
            ViewDmlSourceMatch::TargetOnly => None,
            ViewDmlSourceMatch::Source(source) => Some(source),
        };
        let evaluation_row = source_context.as_ref().map_or_else(
            || physical.clone(),
            |source| dml_join_rows(&physical, source),
        );
        let mut row = PendingViewUpdate {
            new: old.clone(),
            old,
            source_context,
            evaluation_row,
            evaluated_assignments: BTreeSet::new(),
        };
        evaluate_view_update_assignments(
            read_engine,
            &target,
            stmt,
            condition_columns.as_ref(),
            &mut row,
            params,
            &snapshot,
        )?;
        pending.push(row);
    }
    let rule_rows = pending
        .iter()
        .map(|row| {
            Ok(crate::sql::rules::RuleRowImage {
                old_storage_table: None,
                old_doc_id: None,
                old: Some(view_document(&target, &row.old)?),
                new_storage_table: None,
                new_doc_id: None,
                new: Some(view_document(&target, &row.new)?),
                context: row.source_context.clone(),
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut rule_batch = crate::sql::rules::prepare_rule_batch(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Update,
        rule_rows,
    )?;
    let update_qualification_count = source_update_qualification_count
        .or(row_independent_update_qualification)
        .unwrap_or_else(|| rule_batch.event_row_count());
    rule_batch.set_action_qualification_count(update_qualification_count);
    if !original_query_survives {
        let action_columns = rule_batch.matched_action_row_columns();
        for (row, required) in pending.iter_mut().zip(&action_columns) {
            evaluate_view_update_assignments(
                read_engine,
                &target,
                stmt,
                Some(required),
                row,
                params,
                &snapshot,
            )?;
        }
        rule_batch.supplement_rows(
            pending
                .iter()
                .map(|row| {
                    Ok(crate::sql::rules::RuleRowImage {
                        old_storage_table: None,
                        old_doc_id: None,
                        old: Some(view_document(&target, &row.old)?),
                        new_storage_table: None,
                        new_doc_id: None,
                        new: Some(view_document(&target, &row.new)?),
                        context: row.source_context.clone(),
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
        )?;
        let outer_rule_rows = pending
            .iter()
            .map(|row| {
                Ok(crate::sql::rules::RuleRowImage {
                    old_storage_table: None,
                    old_doc_id: None,
                    old: Some(view_document(&target, &row.old)?),
                    new_storage_table: None,
                    new_doc_id: None,
                    new: Some(view_document(&target, &row.new)?),
                    context: row.source_context.clone(),
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let mut outer_rule_batches =
            super::prepare_view_rule_batches(super::ViewRuleBatchRequest {
                engine,
                relations: &stmt.view_rule_relations,
                event: uqa_sql::ast::RuleEvent::Update,
                rows: &outer_rule_rows,
                params,
                scope: &snapshot,
                insert_plans: &[],
                update_plans: &stmt.view_rule_update_plans,
                document_relation: Some(&target.canonical_name),
            })?;
        outer_rule_batches.configure_action_qualification(Some(update_qualification_count));
        let outer_outcome = outer_rule_batches
            .execute_actions_with_affected(engine, stmt.view_rule_returning.as_ref())?;
        let outcome = rule_batch.execute_actions_with_affected(
            engine,
            crate::sql::rules::RuleReturningRequest::from_plan(
                &stmt.returning,
                &stmt.returning_aliases,
                &stmt.subqueries,
            ),
        )?;
        if outcome.returning.is_some() && outer_outcome.returning.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot have RETURNING lists in multiple rules".into(),
            });
        }
        if let Some(returning) = outcome.returning {
            return returning.project(
                engine,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &ctes,
                    supplemental_schema: source_rows
                        .as_ref()
                        .map(uqa_execution::SharedSpill::row_schema),
                },
            );
        }
        if let Some(returning) = outer_outcome.returning {
            return returning.project(
                engine,
                params,
                &ctes,
                source_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            );
        }
        return finish_view_dml(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: source_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            },
            Vec::new(),
            0,
        );
    }
    let rule_returning = rule_batch.execute_actions(
        engine,
        crate::sql::rules::RuleReturningRequest::from_plan(
            &stmt.returning,
            &stmt.returning_aliases,
            &stmt.subqueries,
        ),
    )?;
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();
    for (index, row) in pending.into_iter().enumerate() {
        if rule_batch.suppresses(index) {
            continue;
        }
        let Some(final_new) = crate::sql::triggers::fire_instead_of_row_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerEvent::Update,
            Some(&row.old),
            Some(&row.new),
            &assigned_columns,
        )?
        else {
            continue;
        };
        affected += 1;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_value_row(
                engine,
                ReturningValueProjectionRow {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    current: &final_new,
                    old: Some(&row.old),
                    new: Some(&final_new),
                    aliases: &stmt.returning_aliases,
                    context: row.source_context.as_ref(),
                },
                &stmt.returning,
                params,
                &ctes,
            )?);
        }
    }
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &target.canonical_name,
        uqa_sql::ast::TriggerTiming::After,
        uqa_sql::ast::TriggerEvent::Update,
        &assigned_columns,
    )?;
    let result = finish_view_dml(
        engine,
        DmlReturningShape {
            table: &target.canonical_name,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes: &ctes,
            supplemental_schema: source_rows
                .as_ref()
                .map(uqa_execution::SharedSpill::row_schema),
        },
        returning_rows,
        affected,
    )?;
    if let Some(rule_returning) = rule_returning {
        return rule_returning.project(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: source_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            },
        );
    }
    Ok(result)
}

pub(super) fn run_view_delete_inner(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let target = resolve_view_target(engine, &stmt.table)?;
    if stmt.source.is_none() {
        let allowed = BTreeSet::from([stmt.target_qualifier.clone()]);
        if let Some(predicate) = stmt.predicate.as_ref() {
            validate_dml_expression_qualifiers(predicate, &allowed)?;
        }
    }
    let original_query_survives = !crate::sql::rules::relation_suppresses_original_query(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Delete,
    )?;
    let has_before_statement_trigger = original_query_survives
        && !engine
            .triggers_for(
                &target.canonical_name,
                uqa_sql::ast::TriggerTiming::Before,
                uqa_sql::ast::TriggerEvent::Delete,
                false,
                &[],
            )?
            .is_empty();
    let statement_snapshot = has_before_statement_trigger
        .then(|| engine.capture_statement_snapshot_engine())
        .transpose()?;
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::Before,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
        )?;
    }
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine();
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let row_independent_delete_qualification = if stmt.source.is_none()
        && !engine
            .rules_for(&target.canonical_name, uqa_sql::ast::RuleEvent::Delete)?
            .is_empty()
    {
        super::row_independent_mutation_qualification_count(
            read_engine,
            stmt.predicate.as_ref(),
            params,
            &ctes,
        )?
    } else {
        None
    };
    if stmt.view_rule_relations.is_empty()
        && !original_query_survives
        && !crate::sql::rules::relation_rules_require_event_rows(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Delete,
        )?
    {
        validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
        let rule_batch = crate::sql::rules::prepare_rule_batch(
            engine,
            &target.canonical_name,
            uqa_sql::ast::RuleEvent::Delete,
            Vec::new(),
        )?;
        let outcome = rule_batch.execute_actions_with_affected(
            engine,
            crate::sql::rules::RuleReturningRequest::from_plan(
                &stmt.returning,
                &stmt.returning_aliases,
                &stmt.subqueries,
            ),
        )?;
        if let Some(returning) = outcome.returning {
            return returning.project(
                engine,
                DmlReturningShape {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &ctes,
                    supplemental_schema: None,
                },
            );
        }
        return finish_view_dml(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: None,
            },
            Vec::new(),
            0,
        );
    }
    let mut source_scope = ctes.returning_statement_snapshot_scope();
    let source_rows = stmt
        .source
        .as_deref()
        .map(|source| build_join_spill_with_ctes(read_engine, source, params, &mut source_scope))
        .transpose()?;
    validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        source_rows
            .as_ref()
            .map(uqa_execution::SharedSpill::row_schema),
    )?;
    let mut target_scope = ctes.returning_statement_snapshot_scope();
    let required_columns = if !original_query_survives && stmt.view_rule_relations.is_empty() {
        required_view_delete_columns(engine, &target, stmt)?
    } else {
        None
    };
    let candidates = materialize_view_rows(
        read_engine,
        &target,
        required_columns.as_ref(),
        params,
        &mut target_scope,
    )?;
    let snapshot = ctes.returning_statement_snapshot_scope();
    let source_delete_qualification_count = source_rows
        .as_ref()
        .map(|source_rows| {
            count_view_source_qualifications(ViewSourceQualification {
                engine: read_engine,
                target: &target,
                target_qualifier: &stmt.target_qualifier,
                predicate: stmt.predicate.as_ref(),
                candidates: &candidates,
                source_rows,
                params,
                ctes: &snapshot,
            })
        })
        .transpose()?;
    let mut pending = Vec::new();
    for old in candidates {
        let physical = target_row(&target, &stmt.target_qualifier, &old)?;
        let Some(source_match) = matching_source_context(
            read_engine,
            &physical,
            source_rows.as_ref(),
            stmt.predicate.as_ref(),
            params,
            &snapshot,
        )?
        else {
            continue;
        };
        let source_context = match source_match {
            ViewDmlSourceMatch::TargetOnly => None,
            ViewDmlSourceMatch::Source(source) => Some(source),
        };
        pending.push((old, source_context));
    }
    let rule_rows = pending
        .iter()
        .map(|(old, source_context)| {
            Ok(crate::sql::rules::RuleRowImage {
                old_storage_table: None,
                old_doc_id: None,
                old: Some(view_document(&target, old)?),
                new_storage_table: None,
                new_doc_id: None,
                new: None,
                context: source_context.clone(),
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut outer_rule_batches = super::prepare_view_rule_batches(super::ViewRuleBatchRequest {
        engine,
        relations: &stmt.view_rule_relations,
        event: uqa_sql::ast::RuleEvent::Delete,
        rows: &rule_rows,
        params,
        scope: &snapshot,
        insert_plans: &[],
        update_plans: &[],
        document_relation: Some(&target.canonical_name),
    })?;
    let mut rule_batch = crate::sql::rules::prepare_rule_batch(
        engine,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Delete,
        rule_rows,
    )?;
    let action_qualification_count = source_delete_qualification_count
        .or(row_independent_delete_qualification)
        .unwrap_or_else(|| rule_batch.event_row_count());
    outer_rule_batches.configure_action_qualification(Some(action_qualification_count));
    rule_batch.set_action_qualification_count(action_qualification_count);
    let outer_rule_outcome = outer_rule_batches
        .execute_actions_with_affected(engine, stmt.view_rule_returning.as_ref())?;
    let rule_outcome = rule_batch.execute_actions_with_affected(
        engine,
        crate::sql::rules::RuleReturningRequest::from_plan(
            &stmt.returning,
            &stmt.returning_aliases,
            &stmt.subqueries,
        ),
    )?;
    if rule_outcome.returning.is_some() && outer_rule_outcome.returning.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();
    for (index, (old, source_context)) in pending.into_iter().enumerate() {
        if rule_batch.suppresses(index) {
            continue;
        }
        if crate::sql::triggers::fire_instead_of_row_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerEvent::Delete,
            Some(&old),
            None,
            &[],
        )?
        .is_none()
        {
            continue;
        }
        affected += 1;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_value_row(
                engine,
                ReturningValueProjectionRow {
                    table: &target.canonical_name,
                    target_qualifier: &stmt.target_qualifier,
                    current: &old,
                    old: Some(&old),
                    new: None,
                    aliases: &stmt.returning_aliases,
                    context: source_context.as_ref(),
                },
                &stmt.returning,
                params,
                &ctes,
            )?);
        }
    }
    if original_query_survives {
        crate::sql::triggers::fire_statement_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerTiming::After,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
        )?;
    }
    let result = finish_view_dml(
        engine,
        DmlReturningShape {
            table: &target.canonical_name,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes: &ctes,
            supplemental_schema: source_rows
                .as_ref()
                .map(uqa_execution::SharedSpill::row_schema),
        },
        returning_rows,
        affected,
    )?;
    if let Some(rule_returning) = rule_outcome.returning {
        return rule_returning.project(
            engine,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &ctes,
                supplemental_schema: source_rows
                    .as_ref()
                    .map(uqa_execution::SharedSpill::row_schema),
            },
        );
    }
    if let Some(rule_returning) = outer_rule_outcome.returning {
        return rule_returning.project(
            engine,
            params,
            &ctes,
            source_rows
                .as_ref()
                .map(uqa_execution::SharedSpill::row_schema),
        );
    }
    Ok(result)
}
