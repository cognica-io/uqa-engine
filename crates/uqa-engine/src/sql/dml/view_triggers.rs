//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-oriented DML execution for views with `INSTEAD OF` triggers.

use super::{
    build_join_spill_with_ctes, build_returning_value_row, dml_join_rows, dml_returning_result,
    eval_mutation_expr, validate_dml_expression_qualifiers, validate_returning_alias_relations,
    BTreeSet, ColumnType, CteScope, DeletePlan, DmlReturningShape, Engine, InsertPlan,
    OwnedPhysicalRow, PhysicalRow, ReturningValueProjectionRow, RowSchema, SQLError, SQLParam,
    SQLResult, ScalarExpr, UpdatePlan, Value,
};

struct ViewDmlTarget {
    canonical_name: String,
    definition: crate::StoredView,
    columns: Vec<String>,
    types: Vec<Option<ColumnType>>,
}

pub(super) fn target_is_view(engine: &Engine, name: &str) -> Result<bool, SQLError> {
    let candidates = engine
        .relation_lookup_candidates(name)
        .map_err(|error| SQLError::Internal(format!("resolve DML relation `{name}`: {error}")))?;
    let tables = engine.storage.tables.read();
    let views = engine.durable.views.read();
    for relation in candidates {
        if tables.contains_key(&relation) {
            return Ok(false);
        }
        if views.contains_key(&relation) {
            return Ok(true);
        }
    }
    Ok(false)
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
    params: &[SQLParam],
    scope: &mut CteScope,
) -> Result<Vec<Vec<Value>>, SQLError> {
    let result = crate::sql::select::execute_query_plan_with_ctes(
        engine,
        &target.definition.query,
        params,
        scope,
    )?;
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
    let has_before_statement_trigger = !engine
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
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &target.canonical_name,
        uqa_sql::ast::TriggerTiming::Before,
        uqa_sql::ast::TriggerEvent::Insert,
        &[],
    )?;
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine();
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let input_rows = if let Some(source) = stmt.source.as_deref() {
        let mut source_scope = ctes.returning_statement_snapshot_scope();
        values_from_result(crate::sql::select::execute_query_plan_with_ctes(
            read_engine,
            source,
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
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();
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
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &target.canonical_name,
        uqa_sql::ast::TriggerTiming::After,
        uqa_sql::ast::TriggerEvent::Insert,
        &[],
    )?;
    finish_view_dml(
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
    )
}

enum ViewDmlSourceMatch {
    TargetOnly,
    Source(OwnedPhysicalRow),
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
    let has_before_statement_trigger = !engine
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
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &target.canonical_name,
        uqa_sql::ast::TriggerTiming::Before,
        uqa_sql::ast::TriggerEvent::Update,
        &assigned_columns,
    )?;
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine();
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
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
    let candidates = materialize_view_rows(read_engine, &target, params, &mut target_scope)?;
    let snapshot = ctes.returning_statement_snapshot_scope();
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();
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
        let mut new = old.clone();
        for assignment in &stmt.assignments {
            let position = target
                .columns
                .iter()
                .position(|column| column == &assignment.column)
                .ok_or_else(|| SQLError::UnknownColumn(assignment.column.clone()))?;
            let value = if matches!(assignment.value, ScalarExpr::Default) {
                Value::Null
            } else {
                eval_mutation_expr(
                    read_engine,
                    &snapshot,
                    &assignment.value,
                    Some(&evaluation_row),
                    params,
                )?
            };
            new[position] = coerce_view_value(&target, position, value)?;
        }
        let Some(final_new) = crate::sql::triggers::fire_instead_of_row_triggers(
            engine,
            &target.canonical_name,
            uqa_sql::ast::TriggerEvent::Update,
            Some(&old),
            Some(&new),
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
                    old: Some(&old),
                    new: Some(&final_new),
                    aliases: &stmt.returning_aliases,
                    context: source_context.as_ref(),
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
    finish_view_dml(
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
    )
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
    let has_before_statement_trigger = !engine
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
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &target.canonical_name,
        uqa_sql::ast::TriggerTiming::Before,
        uqa_sql::ast::TriggerEvent::Delete,
        &[],
    )?;
    let read_engine = statement_snapshot.as_ref().unwrap_or(engine);
    let mut ctes = CteScope::new_for_current_routine();
    crate::sql::select::materialize_plan_ctes(read_engine, &stmt.ctes, params, &mut ctes)?;
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
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
    let candidates = materialize_view_rows(read_engine, &target, params, &mut target_scope)?;
    let snapshot = ctes.returning_statement_snapshot_scope();
    let mut affected = 0_u64;
    let mut returning_rows = Vec::new();
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
    crate::sql::triggers::fire_statement_triggers(
        engine,
        &target.canonical_name,
        uqa_sql::ast::TriggerTiming::After,
        uqa_sql::ast::TriggerEvent::Delete,
        &[],
    )?;
    finish_view_dml(
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
    )
}
