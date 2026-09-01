//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    coerce_to_column_type, ddl_storage_error, eval_lowered_expression, index_vectors_for_type,
    BTreeMap, ColumnType, Document, Engine, RowUpdateVectors, SQLError, Value,
};

pub(super) fn reject_default_change_on_generated_column(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    if crate::sql::generated::generated_column_kind(engine, table, column)?.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "column `{column}` of relation `{table}` is a generated column; use SET EXPRESSION or DROP EXPRESSION"
        )));
    }
    Ok(())
}

pub(super) fn validate_and_rewrite_generated_rows(
    engine: &Engine,
    table: &str,
    rewrite_physical_rows: bool,
) -> Result<(), SQLError> {
    let doc_ids = engine.live_table_doc_ids(table)?;
    let mut rows = Vec::with_capacity(doc_ids.len());
    for doc_id in &doc_ids {
        let Some(mut document) = engine.get_document(table, *doc_id)? else {
            continue;
        };
        crate::sql::generated::refresh_stored_generated_columns(engine, table, &mut document)?;
        rows.push((*doc_id, document));
    }
    validate_key_constraint_rows(engine, table, &rows)?;
    if rewrite_physical_rows {
        let mut replacements = Vec::with_capacity(rows.len());
        let mut remaps_primary_key = false;
        for (old_doc_id, document) in rows {
            let new_doc_id = crate::sql::dml::integer_primary_key_doc_id(engine, table, &document)?
                .unwrap_or(old_doc_id);
            remaps_primary_key |= new_doc_id != old_doc_id;
            replacements.push((old_doc_id, new_doc_id, document));
        }
        if remaps_primary_key {
            for (old_doc_id, _, _) in &replacements {
                engine.delete_document(table, *old_doc_id)?;
            }
        }
        for (old_doc_id, new_doc_id, document) in replacements {
            let vectors = crate::sql::dml::document_vectors(engine, table, &document)?;
            engine.add_prepared_document_with_vector_values(
                table,
                if remaps_primary_key {
                    new_doc_id
                } else {
                    old_doc_id
                },
                document,
                vectors,
                remaps_primary_key,
            )?;
            if remaps_primary_key {
                engine
                    .advance_next_id(table, new_doc_id)
                    .map_err(|error| ddl_storage_error("generated primary key rewrite", error))?;
            }
        }
    }
    validate_all_table_rows(engine)
}

pub(super) fn validate_key_constraint_rows(
    engine: &Engine,
    table: &str,
    rows: &[(uqa_core::DocId, Document)],
) -> Result<(), SQLError> {
    for constraint in engine
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("generated-column validation", error))?
    {
        let mut seen = std::collections::BTreeSet::new();
        for (_, document) in rows {
            let values = constraint
                .columns
                .iter()
                .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>();
            let contains_null = values.iter().any(|value| matches!(value, Value::Null));
            if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey && contains_null
            {
                return Err(SQLError::TypeMismatch(format!(
                    "PRIMARY KEY constraint contains NULL values on table `{table}`"
                )));
            }
            if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
                && contains_null
                && !constraint.nulls_not_distinct
            {
                continue;
            }
            if !seen.insert(values) {
                return Err(SQLError::TypeMismatch(format!(
                    "{} constraint would be violated by generated values on table `{table}`",
                    match constraint.kind {
                        uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
                        uqa_sql::ast::TableKeyConstraintKind::Unique => "UNIQUE",
                    }
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_all_table_rows(engine: &Engine) -> Result<(), SQLError> {
    for table in engine
        .table_names()
        .map_err(|error| ddl_storage_error("generated-column validation", error))?
    {
        for doc_id in engine.live_table_doc_ids(&table)? {
            let Some(document) = engine.get_document(&table, doc_id)? else {
                continue;
            };
            crate::sql::dml::validate_document_constraints(
                engine,
                &table,
                &document,
                &[],
                Some(doc_id),
            )?;
        }
    }
    Ok(())
}

pub(super) fn ensure_column_exists(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    if engine
        .try_table_has_column(table, column)
        .map_err(|err| ddl_storage_error("ALTER COLUMN", err))?
    {
        Ok(())
    } else {
        Err(SQLError::Unsupported(format!(
            "ALTER TABLE ALTER COLUMN: column `{column}` does not exist"
        )))
    }
}

pub(super) fn ensure_existing_values_not_null(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    let mut null_rows = 0usize;
    for doc_id in engine.live_table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
            continue;
        };
        if matches!(doc.get(column), None | Some(Value::Null)) {
            null_rows += 1;
        }
    }
    if null_rows > 0 {
        return Err(SQLError::TypeMismatch(format!(
            "ALTER TABLE ALTER COLUMN: column `{column}` contains NULL values"
        )));
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
pub(super) fn validate_added_key_constraint(
    engine: &Engine,
    table: &str,
    constraint: &uqa_sql::ast::TableKeyConstraint,
) -> Result<(), SQLError> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let column_names: std::collections::BTreeSet<&str> =
        columns.iter().map(|column| column.name.as_str()).collect();
    for column in &constraint.columns {
        if !column_names.contains(column.as_str()) {
            return Err(SQLError::TypeMismatch(format!(
                "ALTER TABLE ADD CONSTRAINT references unknown column `{column}`"
            )));
        }
    }
    if constraint.without_overlaps {
        let period_column = constraint.columns.last().ok_or_else(|| {
            SQLError::TypeMismatch(
                "constraint using WITHOUT OVERLAPS needs at least two columns".into(),
            )
        })?;
        let period_type = columns
            .iter()
            .find(|column| column.name == *period_column)
            .map(|column| &column.ty)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{period_column}")))?;
        if !matches!(
            period_type,
            ColumnType::Range(_) | ColumnType::Multirange(_)
        ) {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "column \"{period_column}\" in WITHOUT OVERLAPS is not a range or multirange type"
                ),
            });
        }
        if constraint.columns.len() < 2 {
            return Err(SQLError::TypeMismatch(
                "constraint using WITHOUT OVERLAPS needs at least two columns".into(),
            ));
        }
    }

    let existing_keys = engine
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
    if let Some(name) = constraint.name.as_deref() {
        let check_name_exists = engine
            .try_check_constraint_definitions(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        let foreign_name_exists = engine
            .try_foreign_keys(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        let key_name_exists = existing_keys
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        if check_name_exists || foreign_name_exists || key_name_exists {
            return Err(SQLError::TypeMismatch(format!(
                "constraint `{name}` already exists on table `{table}`"
            )));
        }
    }
    if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey
        && existing_keys
            .iter()
            .any(|existing| existing.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey)
    {
        return Err(SQLError::TypeMismatch(format!(
            "multiple PRIMARY KEY constraints are not allowed on table `{table}`"
        )));
    }

    let mut seen = std::collections::BTreeSet::<Vec<Value>>::new();
    for doc_id in engine.live_table_doc_ids(table)? {
        let Some(document) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let values: Vec<Value> = constraint
            .columns
            .iter()
            .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
            .collect();
        let contains_null = values.iter().any(|value| matches!(value, Value::Null));
        if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey && contains_null {
            return Err(SQLError::TypeMismatch(format!(
                "PRIMARY KEY constraint contains NULL values on table `{table}`"
            )));
        }
        if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
            && contains_null
            && !constraint.nulls_not_distinct
        {
            continue;
        }
        if constraint.without_overlaps {
            if crate::sql::dml::without_overlaps_conflict(
                engine,
                table,
                constraint,
                &document,
                Some(doc_id),
            )? {
                return Err(SQLError::Routine {
                    sqlstate: "23P01".into(),
                    message: format!(
                        "could not create constraint because relation \"{table}\" contains overlapping key values"
                    ),
                });
            }
            continue;
        }
        if !seen.insert(values) {
            return Err(SQLError::Routine {
                sqlstate: "23505".into(),
                message: format!(
                    "{} constraint would be violated by duplicate values on table `{table}`",
                    match constraint.kind {
                        uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
                        uqa_sql::ast::TableKeyConstraintKind::Unique => "UNIQUE",
                    }
                ),
            });
        }
    }
    Ok(())
}

/// Apply the new column's default to rows that existed before `ADD COLUMN`.
/// `PostgreSQL` stores one missing value for a non-volatile default, including on an empty table, while volatile defaults are evaluated independently for every existing row and do not populate `attmissingval`.
pub(super) fn backfill_added_column(
    engine: &Engine,
    table: &str,
    column: &str,
    default_expr: Option<&uqa_sql::ast::Expr>,
    not_null: bool,
) -> Result<Option<Value>, SQLError> {
    let doc_ids = engine.live_table_doc_ids(table)?;
    let Some(default_expr) = default_expr else {
        if not_null && !doc_ids.is_empty() {
            return Err(SQLError::Routine {
                sqlstate: "23502".into(),
                message: format!(
                    "column \"{column}\" of relation \"{table}\" contains null values"
                ),
            });
        }
        return Ok(None);
    };
    let column_type = engine
        .column_type(table, column)
        .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?;
    let lowered = uqa_planner::ExpressionPlan::lower(default_expr.clone());
    let volatile = crate::sql::volatility::expr_contains_volatile_function(engine, &lowered.scalar);
    if volatile {
        for doc_id in doc_ids {
            let value = eval_lowered_expression(engine, default_expr, None, &[])?;
            let value = coerce_to_column_type(engine, table, column, value)?;
            if not_null && value == Value::Null {
                return Err(SQLError::Routine {
                    sqlstate: "23502".into(),
                    message: format!(
                        "null value in column \"{column}\" of relation \"{table}\" violates not-null constraint"
                    ),
                });
            }
            let mut vectors: RowUpdateVectors = BTreeMap::new();
            if let Some(ty) = column_type
                .as_ref()
                .filter(|ty| matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)))
            {
                vectors.insert(column.to_string(), index_vectors_for_type(&value, ty)?);
            }
            engine.update_document_fields_with_vector_values(
                table,
                doc_id,
                BTreeMap::from([(column.to_string(), value)]),
                vectors,
            )?;
        }
        for definition in engine.require_table(table)?.columns.write().iter_mut() {
            definition.missing_value = None;
        }
        return Ok(None);
    }
    let default_value = coerce_to_column_type(
        engine,
        table,
        column,
        eval_lowered_expression(engine, default_expr, None, &[])?,
    )?;
    if not_null && default_value == Value::Null && !doc_ids.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "23502".into(),
            message: format!(
                "null value in column \"{column}\" of relation \"{table}\" violates not-null constraint"
            ),
        });
    }
    let vector_value = match column_type.as_ref() {
        Some(ty) if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) => {
            Some(index_vectors_for_type(&default_value, ty)?)
        }
        Some(_) | None => None,
    };
    for doc_id in doc_ids {
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), default_value.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if let Some(v) = vector_value.as_ref() {
            vectors.insert(column.to_string(), v.clone());
        }
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors)?;
    }
    Ok((default_value != Value::Null).then_some(default_value))
}

// DDL
