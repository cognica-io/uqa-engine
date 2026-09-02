//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::scalar::{eval_lowered_expression, eval_physical_scalar, PhysicalEvalContext};
use super::ScopedEngineHook;
use super::{
    bind_projection_output_schema, build_join_spill_with_ctes,
    build_projection_physical_row_with_ctes, coerce_to_column_type, column_type_name, doc_id_value,
    partition_insert_target, validate_vector_dimensions, value_to_tensor, value_to_vector,
    BTreeMap, BTreeSet, BinaryOp, ColumnType, CteScope, DocId, Document, Engine, ForeignKey,
    ForeignKeyAction, ForeignKeyMatch, RowIndependentUpdateValues, SQLError, SQLParam, SQLResult,
    Value, DOC_ID_COLUMN, TABLE_OID_COLUMN, XMIN_COLUMN, XMIN_STORAGE_COLUMN,
    XMIN_USER_STORAGE_COLUMN,
};
use crate::RelationIdentity;
use uqa_execution::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr};
use uqa_planner::{
    ComputePlan, ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan,
    MergeWhenPlan, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan, UpdatePlan,
    ViewCheckPlan, ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan,
};

mod protocol;
mod view_rules;

pub(in crate::sql) use protocol::*;
pub(crate) use protocol::{
    CommandExactIndex, CommandMutationOverlay, DeferredForeignKeyCheck, TransactionRowChange,
};
use view_rules::{prepare_view_rule_batches, ViewRuleBatchRequest};

fn prune_unused_query_outputs(
    query: &mut QueryPlan,
    required_positions: &BTreeSet<usize>,
    expected_width: usize,
) {
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        return;
    };
    let projection_can_be_pruned = matches!(block.compute, ComputePlan::Project)
        && !block.distinct
        && block.distinct_on.is_empty()
        && block.order_by.is_empty()
        && block.projections.len() == expected_width;
    if !projection_can_be_pruned {
        return;
    }
    for (position, projection) in block.projections.iter_mut().enumerate() {
        if !required_positions.contains(&position) {
            projection.expr = ScalarExpr::Literal(Value::Null);
        }
    }
}

fn dml_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

/// Resolve a statement's mutation target once, before any internal storage or rewrite path can observe its textual name.
fn resolve_dml_target_name(
    engine: &Engine,
    name: &str,
    target_relation_bound: bool,
) -> Result<String, SQLError> {
    let resolution = if target_relation_bound {
        engine.resolve_bound_relation_kind(name)?.into_found()
    } else {
        engine.try_resolve_visible_relation_kind(name)?
    };
    resolution
        .map(|(canonical, _)| canonical)
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
}

pub(crate) fn update_lock_strength(
    engine: &Engine,
    table: &str,
    columns: &[String],
) -> uqa_sql::ast::LockStrength {
    let Ok(keys) = engine.try_key_constraints(table) else {
        return uqa_sql::ast::LockStrength::ForUpdate;
    };
    let Ok(Some(definitions)) = engine.try_describe_table(table) else {
        return uqa_sql::ast::LockStrength::ForUpdate;
    };
    let assigned = columns.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let touches_key = keys.iter().any(|constraint| {
        constraint.columns.iter().any(|column| {
            if assigned.contains(column.as_str()) {
                return true;
            }
            let Some(generated) = definitions
                .iter()
                .find(|definition| definition.name == *column)
                .and_then(|definition| definition.generated.as_ref())
            else {
                return false;
            };
            let mut dependencies = BTreeSet::new();
            let expression = uqa_planner::ExpressionPlan::lower((*generated.expression).clone());
            !expression.scalar.collect_columns(&mut dependencies)
                || dependencies
                    .iter()
                    .any(|dependency| assigned.contains(dependency.as_str()))
        })
    });
    if touches_key {
        uqa_sql::ast::LockStrength::ForUpdate
    } else {
        uqa_sql::ast::LockStrength::ForNoKeyUpdate
    }
}

fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}

fn is_virtual_document_id_column(column: &str, definitions: &[uqa_sql::ast::ColumnDef]) -> bool {
    column == DOC_ID_COLUMN
        && !definitions
            .iter()
            .any(|definition| definition.name == DOC_ID_COLUMN)
}

pub(crate) fn stamp_tuple_xmin(
    engine: &Engine,
    table: &str,
    document: &mut Document,
) -> Result<(), SQLError> {
    let xmin = Value::Int(i64::from(engine.tuple_version_xid()?));
    let previous_system_xmin = document.get(XMIN_STORAGE_COLUMN).cloned();
    let schemaless_user_xmin_marked = document
        .get(XMIN_USER_STORAGE_COLUMN)
        .is_some_and(|value| value == &Value::Bool(true));
    let public_xmin_was_system_mirror = previous_system_xmin
        .as_ref()
        .is_some_and(|previous| document.get(XMIN_COLUMN) == Some(previous));
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("resolve tuple-version schema", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let has_declared_xmin = definitions
        .iter()
        .any(|definition| definition.name == XMIN_COLUMN);
    let schemaless_user_xmin = definitions.is_empty()
        && (schemaless_user_xmin_marked
            || (document.contains_key(XMIN_COLUMN)
                && (previous_system_xmin.is_none() || !public_xmin_was_system_mirror)));
    document.insert(XMIN_STORAGE_COLUMN.into(), xmin.clone());
    if schemaless_user_xmin {
        document.insert(XMIN_USER_STORAGE_COLUMN.into(), Value::Bool(true));
    } else {
        document.remove(XMIN_USER_STORAGE_COLUMN);
    }
    if !has_declared_xmin && !schemaless_user_xmin {
        // Keep the legacy projection mirror while old database rows are migrated lazily. The collision-free key above is authoritative and the mirror is never written when `xmin` belongs to the user schema.
        document.insert(XMIN_COLUMN.into(), xmin);
    }
    Ok(())
}

fn dml_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    dml_target_row_for_storage(engine, table, table, qualifier, doc_id, document)
}

fn dml_target_row_for_storage(
    engine: &Engine,
    table: &str,
    storage_table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    dml_target_row_for_storage_optional(
        engine,
        table,
        Some(storage_table),
        qualifier,
        Some(doc_id),
        document,
        None,
    )
}

fn dml_target_row_for_storage_optional(
    engine: &Engine,
    table: &str,
    storage_table: Option<&str>,
    qualifier: &str,
    doc_id: Option<DocId>,
    document: &Document,
    selected_columns: Option<&BTreeSet<String>>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut materialized = document.clone();
    if let Some(selected_columns) = selected_columns {
        crate::engine_generated::materialize_selected_virtual_generated_columns(
            &definitions,
            &mut materialized,
            selected_columns,
        )?;
    } else {
        crate::engine_generated::materialize_virtual_generated_columns(
            &definitions,
            &mut materialized,
        )?;
    }
    let mut columns = if definitions.is_empty() {
        materialized
            .keys()
            .filter(|column| {
                column.as_str() != XMIN_STORAGE_COLUMN
                    && column.as_str() != XMIN_USER_STORAGE_COLUMN
                    && column.as_str() != XMIN_COLUMN
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(ColumnType::Oid));
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    let values = columns
        .iter()
        .map(|column| {
            if is_virtual_document_id_column(column, &definitions)
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id.map_or(Ok(Value::Null), doc_id_value)
            } else if column == TABLE_OID_COLUMN {
                storage_table.map_or(Ok(Value::Null), |storage_table| {
                    Ok(Value::Int(crate::sql::catalog::table_relation_oid(
                        engine,
                        storage_table,
                    )?))
                })
            } else if column == XMIN_COLUMN {
                Ok(materialized
                    .get(XMIN_STORAGE_COLUMN)
                    .cloned()
                    .unwrap_or(Value::Null))
            } else {
                Ok(materialized.get(column).cloned().unwrap_or(Value::Null))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::from_values(values),
    ))
}

struct ViewCheckContext<'a> {
    engine: &'a Engine,
    table: &'a str,
    storage_table: &'a str,
    target_qualifier: &'a str,
    doc_id: DocId,
    document: &'a Document,
    checks: &'a [ViewCheckPlan],
    params: &'a [SQLParam],
    scope: &'a CteScope,
}

fn validate_view_checks(context: ViewCheckContext<'_>) -> Result<(), SQLError> {
    let ViewCheckContext {
        engine,
        table,
        storage_table,
        target_qualifier,
        doc_id,
        document,
        checks,
        params,
        scope,
    } = context;
    if checks.is_empty() {
        return Ok(());
    }
    let row = dml_target_row_for_storage(
        engine,
        table,
        storage_table,
        target_qualifier,
        doc_id,
        document,
    )?;
    for check in checks {
        let value = eval_mutation_expr(engine, scope, &check.predicate, Some(&row), params)?;
        if !uqa_sql::expr::truthy(&value) {
            return Err(SQLError::Routine {
                sqlstate: "44000".into(),
                message: format!(
                    "new row violates check option for view \"{}\"",
                    RelationIdentity::from_legacy_name(&check.view)
                        .map_or_else(|_| check.view.clone(), |relation| relation.name)
                ),
            });
        }
    }
    Ok(())
}

fn dml_null_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut columns = if definitions.is_empty() {
        engine
            .try_table_columns(table)
            .map_err(|error| dml_storage_error("DML row schema lookup", error))?
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(ColumnType::Oid));
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    let width = columns.len();
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::nulls(width),
    ))
}

fn dml_join_rows(left: &OwnedPhysicalRow, right: &OwnedPhysicalRow) -> OwnedPhysicalRow {
    OwnedPhysicalRow::new(
        RowSchema::join(&left.schema, &right.schema, std::iter::empty()),
        PhysicalRow::concat(&left.row, &right.row),
    )
}

fn validate_dml_expression_qualifiers(
    expression: &ScalarExpr,
    allowed: &BTreeSet<String>,
) -> Result<(), SQLError> {
    for qualifier in crate::sql::select::expr_qualifiers(expression) {
        if !allowed.contains(&qualifier) {
            return Err(SQLError::UnknownTable(qualifier));
        }
    }
    Ok(())
}

fn dml_append_hidden_qualified_row(
    base: &OwnedPhysicalRow,
    qualifier: &str,
    columns: &[String],
    types: &[Option<ColumnType>],
    values: Vec<Value>,
) -> OwnedPhysicalRow {
    let hidden_types = columns
        .iter()
        .enumerate()
        .map(|(position, _)| types.get(position).cloned().flatten())
        .collect::<Vec<_>>();
    let schema = RowSchema::append_hidden_typed(&base.schema, &hidden_types);
    let offset = base.schema.physical_width();
    let aliases = columns
        .iter()
        .enumerate()
        .map(|(position, column)| {
            (
                ColumnIdentity::qualified(qualifier, column),
                offset + position,
                hidden_types[position].clone(),
            )
        })
        .collect::<Vec<_>>();
    OwnedPhysicalRow::new(
        RowSchema::with_physical_identity_aliases(&schema, &aliases),
        base.row.clone().append_values(values),
    )
}

fn insert_identity_columns(
    engine: &Engine,
    table: &str,
    action: &str,
) -> Result<(Option<String>, String, bool), SQLError> {
    let auto_increment = engine
        .auto_increment_column(table)
        .map_err(|err| dml_storage_error(action, err))?;
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let primary_keys = definitions
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let primary_key = (primary_keys.len() == 1).then(|| primary_keys[0].clone());
    let accepts_supplied_identity =
        auto_increment.is_some() || primary_key.is_some() || definitions.is_empty();
    let id_column = auto_increment
        .clone()
        .or(primary_key)
        .unwrap_or_else(|| "id".into());
    // The conventional `id` field is a storage identity only for the legacy
    // schema-less document API. A declared SQL table without an identity or
    // primary key may contain duplicate ordinary `id` values, so using that
    // field as the physical document key would silently replace rows.
    Ok((auto_increment, id_column, accepts_supplied_identity))
}

fn prepare_auto_increment_identity(
    engine: &Engine,
    table: &str,
    id_column: &str,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<Option<(DocId, bool)>, SQLError> {
    let Some(auto_id_column) = auto_id_column else {
        return Ok(None);
    };
    let definitions = engine
        .auto_increment_columns(table)
        .map_err(|error| dml_storage_error(action, error))?;
    let mut selected_generated = false;
    for (column, provenance) in &definitions {
        if !provenance.is_identity() {
            continue;
        }
        let supplied = document
            .get(column)
            .is_some_and(|value| !matches!(value, Value::Null));
        if supplied {
            if provenance.kind == uqa_sql::ast::AutoIncrementKind::IdentityAlways {
                return Err(SQLError::Routine {
                    sqlstate: "428C9".into(),
                    message: format!(
                        "cannot insert a non-DEFAULT value into identity column \"{column}\""
                    ),
                });
            }
            continue;
        }
        let sequence = provenance.sequence.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "identity column `{table}.{column}` has no durable sequence binding"
            ))
        })?;
        let value = engine.nextval_sql(sequence)?;
        document.insert(
            column.clone(),
            crate::sql::coerce_to_column_type(engine, table, column, Value::Int(value))?,
        );
        selected_generated |= column == auto_id_column;
    }
    let provenance = definitions
        .iter()
        .find(|(column, _)| column == auto_id_column)
        .map(|(_, provenance)| provenance)
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "auto-increment column `{table}.{auto_id_column}` disappeared"
            ))
        })?;
    match provenance.kind {
        uqa_sql::ast::AutoIncrementKind::Serial => Ok(None),
        uqa_sql::ast::AutoIncrementKind::IdentityAlways
        | uqa_sql::ast::AutoIncrementKind::IdentityByDefault => {
            let mut identity = prepare_insert_identity(
                engine,
                table,
                id_column,
                true,
                Some(auto_id_column),
                document,
                action,
            )?;
            if selected_generated {
                identity.1 = false;
            }
            Ok(Some(identity))
        }
        uqa_sql::ast::AutoIncrementKind::Legacy => {
            let owner = engine.partition_identity_owner(table)?;
            engine.lock_relation(&owner, crate::row_locks::RelationLockMode::RowExclusive)?;
            prepare_insert_identity(
                engine,
                &owner,
                id_column,
                true,
                Some(auto_id_column),
                document,
                action,
            )
            .map(Some)
        }
    }
}

fn persist_auto_increment_identity(
    engine: &Engine,
    table: &str,
    auto_id_column: Option<&str>,
    action: &str,
) -> Result<(), SQLError> {
    let Some(auto_id_column) = auto_id_column else {
        return Ok(());
    };
    let legacy = engine
        .auto_increment_columns(table)
        .map_err(|error| dml_storage_error(action, error))?
        .into_iter()
        .find(|(column, _)| column == auto_id_column)
        .is_some_and(|(_, provenance)| provenance.kind == uqa_sql::ast::AutoIncrementKind::Legacy);
    if !legacy {
        return Ok(());
    }
    let owner = engine.partition_identity_owner(table)?;
    engine
        .persist_next_id(&owner)
        .map_err(|error| dml_storage_error(action, error))
}

fn prepare_insert_identity(
    engine: &Engine,
    allocation_table: &str,
    id_column: &str,
    accepts_supplied_identity: bool,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<(DocId, bool), SQLError> {
    let supplied_id = if accepts_supplied_identity {
        document_supplied_id(document, id_column, auto_id_column == Some(id_column))?
    } else {
        None
    };
    let supplied = supplied_id.is_some();
    let doc_id = match supplied_id {
        Some(doc_id) => doc_id,
        None => engine.allocate_next_id(allocation_table)?,
    };
    if auto_id_column == Some(id_column) {
        document.insert(id_column.to_string(), doc_id_value(doc_id)?);
    }
    engine
        .advance_next_id(allocation_table, doc_id)
        .map_err(|error| dml_storage_error(action, error))?;
    Ok((doc_id, supplied))
}

fn validate_mutation_columns<'a>(
    engine: &Engine,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    // Programmatically-created document tables intentionally have no SQL
    // schema and retain their open-field behavior. SQL CREATE TABLE always
    // supplies definitions, and those targets must reject misspelled or
    // repeated mutation columns instead of persisting arbitrary fields.
    if definitions.is_empty() {
        return Ok(());
    }
    let known: BTreeSet<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column) {
            return Err(SQLError::TypeMismatch(format!(
                "{action}: column `{column}` is specified more than once"
            )));
        }
        if !known.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{table}.{column}")));
        }
    }
    Ok(())
}

fn eval_mutation_expr(
    engine: &Engine,
    ctes: &CteScope,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let empty_schema = RowSchema::default();
    let schema = row.map_or(&empty_schema, |row| &row.schema);
    uqa_execution::scalar_type_with_resolver(expression, schema, params, engine)?;
    let expression = uqa_execution::bind_type_introspection_with_resolver(
        expression.clone(),
        schema,
        params,
        engine,
    );
    if let Some(row) = row {
        let view = row.view();
        let context = PhysicalEvalContext::from_row_lookup(&view, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook)
            .with_physical_outer_row(&row.schema, &row.row);
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    } else {
        let context = PhysicalEvalContext::new(None, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    }
}

fn row_independent_mutation_qualification_count(
    engine: &Engine,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<usize>, SQLError> {
    let Some(predicate) = predicate else {
        return Ok(Some(1));
    };
    let mut columns = BTreeSet::new();
    if !predicate.collect_columns(&mut columns) || !columns.is_empty() {
        return Ok(None);
    }
    Ok(Some(usize::from(uqa_sql::expr::truthy(
        &eval_mutation_expr(engine, ctes, predicate, None, params)?,
    ))))
}

struct MutationAssignmentTarget<'a> {
    table: &'a str,
    column: &'a str,
    action: &'a str,
}

fn eval_mutation_assignment(
    engine: &Engine,
    ctes: &CteScope,
    target: MutationAssignmentTarget<'_>,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let MutationAssignmentTarget {
        table,
        column,
        action,
    } = target;
    let generated = crate::sql::generated::generated_column_kind(engine, table, column)?;
    if matches!(expression, ScalarExpr::Default) {
        if generated.is_some() {
            return Ok(None);
        }
        let value = match engine
            .try_column_default_expr(table, column)
            .map_err(|error| dml_storage_error(action, error))?
        {
            Some(default) => eval_lowered_expression(engine, &default, None, params)?,
            None => Value::Null,
        };
        return coerce_to_column_type(engine, table, column, value).map(Some);
    }
    if generated.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "column `{column}` is a generated column; only DEFAULT may be assigned"
        )));
    }
    let value = eval_mutation_expr(engine, ctes, expression, row, params)?;
    coerce_to_column_type(engine, table, column, value).map(Some)
}

fn eval_view_rule_update_assignment(
    engine: &Engine,
    ctes: &CteScope,
    stmt: &UpdatePlan,
    assignment_position: usize,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let value = if matches!(expression, ScalarExpr::Default) {
        Value::Null
    } else {
        eval_mutation_expr(engine, ctes, expression, row, params)?
    };
    for plan in &stmt.view_rule_update_plans {
        let Some(column) = plan.assigned_columns.get(assignment_position) else {
            continue;
        };
        let definition = engine
            .view_definition(&plan.relation)?
            .ok_or_else(|| SQLError::UnknownTable(plan.relation.clone()))?;
        let schema = engine.stored_view_schema(&definition)?;
        let Some(position) =
            schema
                .columns()
                .iter()
                .enumerate()
                .find_map(|(position, internal)| {
                    let public = schema.public_name(position).unwrap_or(internal);
                    public.eq_ignore_ascii_case(column).then_some(position)
                })
        else {
            return Err(SQLError::UnknownColumn(format!(
                "{}.{}",
                plan.relation, column
            )));
        };
        return match schema.column_type(position) {
            Some(ty) => {
                crate::sql::convert_value_to_column_type_with_engine(engine, value, ty).map(Some)
            }
            None => Ok(Some(value)),
        };
    }
    Ok(Some(value))
}

mod conflict;
mod constraints;
mod delete;
mod insert;
mod merge;
mod update;
mod update_from;
mod vectors;
pub(in crate::sql) mod view_automatic;
mod view_triggers;

pub(in crate::sql) use conflict::*;
pub(crate) use constraints::*;
pub(in crate::sql) use delete::*;
pub(in crate::sql) use insert::*;
pub(in crate::sql) use merge::*;
pub(in crate::sql) use update::*;
pub(in crate::sql) use update_from::*;
pub(in crate::sql) use vectors::*;

pub(in crate::sql) fn cursor_command_returning_schema(
    engine: &Engine,
    command: &uqa_planner::CommandPlan,
    params: &[SQLParam],
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    match command {
        uqa_planner::CommandPlan::Merge(plan) => {
            merge_command_returning_schema(engine, plan, params)
        }
        _ => dml_command_returning_schema(engine, command, params),
    }
}
