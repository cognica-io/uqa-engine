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

#[path = "view_triggers/insert.rs"]
mod insert;
mod merge;

pub(super) use insert::run_view_insert_inner;
pub(super) use merge::run_view_merge_inner;

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
    let privilege_subject = if target.definition.security_invoker() {
        scope.privilege_subject()?.to_string()
    } else {
        target.definition.role_owner.clone()
    };
    let mut privilege_scope = scope.enter_privilege_subject(privilege_subject);
    let result = crate::sql::select::execute_query_plan_with_ctes(
        engine,
        &query,
        params,
        &mut privilege_scope,
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
    engine: &Engine,
    target: &ViewDmlTarget,
    position: usize,
    value: Value,
) -> Result<Value, SQLError> {
    match target.types[position].as_ref() {
        Some(ty) => crate::sql::convert_value_to_column_type_with_engine(engine, value, ty),
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

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
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
    let value = coerce_view_value(engine, target, target_position, value)?;
    values[target_position] = Some(value.clone());
    Ok(value)
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
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

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
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

mod update_delete;
pub(super) use update_delete::{run_view_delete_inner, run_view_update_inner};
