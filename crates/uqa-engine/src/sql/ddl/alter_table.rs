//! ALTER TABLE schema mutation and existing-row backfill.

use super::{
    coerce_to_column_type, ddl_storage_error, eval_lowered_expression, index_vectors_for_type,
    rewrite_column_values_to_type, AlterTableAction, AlterTableStmt, BTreeMap, ColumnType, Engine,
    RowUpdateVectors, SQLError, SQLResult, Value,
};

pub(in crate::sql) fn run_alter_table(
    engine: &Engine,
    stmt: AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    if matches!(
        &stmt.action,
        AlterTableAction::DropColumn { cascade: true, .. }
    ) {
        return Err(SQLError::Unsupported(
            "ALTER TABLE DROP COLUMN CASCADE is not supported; no schema or data was changed"
                .into(),
        ));
    }
    engine.with_implicit_transaction(move |engine| run_alter_table_inner(engine, stmt))
}

fn run_alter_table_inner(engine: &Engine, mut stmt: AlterTableStmt) -> Result<SQLResult, SQLError> {
    match engine
        .try_resolve_relation_kind(&stmt.table)
        .map_err(|err| ddl_storage_error("ALTER TABLE", err))?
    {
        Some((canonical, "table")) => stmt.table = canonical,
        Some((canonical, kind)) => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE: relation `{canonical}` is a {kind}, not a table"
            )));
        }
        None if stmt.if_exists => return Ok(SQLResult::empty()),
        None => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE: relation `{}` does not exist",
                stmt.table
            )));
        }
    }
    match stmt.action {
        AlterTableAction::AddColumn {
            column,
            if_not_exists,
        } => {
            let col_name = column.name.clone();
            if engine
                .try_table_has_column(&stmt.table, &col_name)
                .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
            {
                if if_not_exists {
                    return Ok(SQLResult::empty());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ADD COLUMN: column `{col_name}` already exists"
                )));
            }
            match column.ty {
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .create_vector_field(&stmt.table, col_name.clone(), dim)
                        .map_err(|err| ddl_storage_error("ALTER TABLE vector field", err))?;
                }
                ColumnType::Text => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, col_name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                _ => {}
            }
            // Capture the default expression and NOT NULL flag before
            // moving the column into the engine so we can backfill any
            // existing rows. PostgreSQL evaluates the default once per
            // existing row at ALTER TABLE time, which keeps NOT NULL
            // constraints satisfiable for non-empty tables.
            let column_not_null = column.not_null;
            engine
                .try_register_column(&stmt.table, column)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
            let default_expr = engine
                .try_column_default_expr(&stmt.table, &col_name)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN default", e))?;
            backfill_added_column(
                engine,
                &stmt.table,
                &col_name,
                default_expr.as_ref(),
                column_not_null,
            )?;
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
        }
        AlterTableAction::AddKeyConstraint { constraint } => {
            validate_added_key_constraint(engine, &stmt.table, &constraint)?;
            engine
                .add_key_constraint(&stmt.table, &constraint)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade: false,
        } => {
            if !engine
                .try_table_has_column(&stmt.table, &name)
                .map_err(|err| ddl_storage_error("ALTER TABLE DROP COLUMN", err))?
            {
                if if_exists {
                    return Ok(SQLResult::empty());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE DROP COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_drop_column(&stmt.table, &name)
                .map_err(|e| ddl_storage_error("ALTER TABLE DROP COLUMN", e))?;
        }
        AlterTableAction::DropColumn { cascade: true, .. } => unreachable!(),
        AlterTableAction::RenameColumn { from, to } => {
            if !engine
                .try_table_has_column(&stmt.table, &from)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{from}` does not exist"
                )));
            }
            if engine
                .try_table_has_column(&stmt.table, &to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{to}` already exists"
                )));
            }
            engine
                .try_rename_column(&stmt.table, &from, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME COLUMN", e))?;
        }
        AlterTableAction::RenameTable { to } => {
            if engine
                .try_has_table(&to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: relation `{to}` already exists"
                )));
            }
            if !engine
                .try_rename_table(&stmt.table, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME", e))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: rename of `{}` failed",
                    stmt.table
                )));
            }
        }
        AlterTableAction::SetDefault { name, default } => {
            if !engine
                .set_column_default(&stmt.table, &name, Some(default))
                .map_err(|err| ddl_storage_error("ALTER COLUMN SET DEFAULT", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::DropDefault { name } => {
            if !engine
                .set_column_default(&stmt.table, &name, None)
                .map_err(|err| ddl_storage_error("ALTER COLUMN DROP DEFAULT", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::SetNotNull { name } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            ensure_existing_values_not_null(engine, &stmt.table, &name)?;
            engine
                .set_column_not_null(&stmt.table, &name, true)
                .map_err(|err| ddl_storage_error("ALTER COLUMN SET NOT NULL", err))?;
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::DropNotNull { name } => {
            if !engine
                .set_column_not_null(&stmt.table, &name, false)
                .map_err(|err| ddl_storage_error("ALTER COLUMN DROP NOT NULL", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::AlterColumnType { name, ty } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            let old_ty = engine
                .column_type(&stmt.table, &name)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?;
            let old_was_vector =
                matches!(old_ty, Some(ColumnType::Vector(_) | ColumnType::Tensor(_)));
            let new_is_vector = matches!(&ty, ColumnType::Vector(_) | ColumnType::Tensor(_));

            // Row rewrites maintain every currently registered vector index.
            // Detach a vector/tensor index before converting its values to a
            // scalar type, otherwise the first converted scalar is fed back
            // into the old vector index. The enclosing ALTER transaction
            // restores both catalog and physical index state if conversion
            // of any row subsequently fails.
            if old_was_vector && !new_is_vector {
                engine
                    .try_drop_vector_indexes_for_column(&stmt.table, &name)
                    .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
            }
            rewrite_column_values_to_type(engine, &stmt.table, &name, &ty)?;
            engine
                .set_column_type(&stmt.table, &name, &ty)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?;
            match ty {
                ColumnType::Text => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .try_rebuild_vector_index_for_column(&stmt.table, &name, dim)
                        .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
                }
                _ => {}
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
    }
    Ok(SQLResult::empty())
}

fn ensure_column_exists(engine: &Engine, table: &str, column: &str) -> Result<(), SQLError> {
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

fn ensure_existing_values_not_null(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    let mut null_rows = 0usize;
    for doc_id in engine.table_doc_ids(table)? {
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

fn validate_added_key_constraint(
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

    let existing_keys = engine
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
    if let Some(name) = constraint.name.as_deref() {
        let check_name_exists = engine
            .try_check_constraints(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
            .iter()
            .any(|(existing, _)| existing.as_deref() == Some(name));
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
    for doc_id in engine.table_doc_ids(table)? {
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
        if !seen.insert(values) {
            return Err(SQLError::TypeMismatch(format!(
                "{} constraint would be violated by duplicate values on table `{table}`",
                match constraint.kind {
                    uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
                    uqa_sql::ast::TableKeyConstraintKind::Unique => "UNIQUE",
                }
            )));
        }
    }
    Ok(())
}

/// Apply the new column's DEFAULT (or NULL) value to every row that
/// existed before the ADD COLUMN. `PostgreSQL` evaluates the default
/// once per existing row at ALTER TABLE time so NOT NULL columns stay
/// consistent on non-empty tables; the UQA-RS implementation mirrors that
/// semantics by sweeping the document store.
fn backfill_added_column(
    engine: &Engine,
    table: &str,
    column: &str,
    default_expr: Option<&uqa_sql::ast::Expr>,
    not_null: bool,
) -> Result<(), SQLError> {
    let doc_ids = engine.table_doc_ids(table)?;
    if doc_ids.is_empty() {
        return Ok(());
    }
    let default_value = if let Some(expr) = default_expr {
        eval_lowered_expression(engine, expr, None, &[])?
    } else if not_null {
        return Err(SQLError::TypeMismatch(format!(
            "ALTER TABLE ADD COLUMN `{column}` is NOT NULL but no DEFAULT supplied; \
             {} existing row(s) would violate the constraint",
            doc_ids.len()
        )));
    } else {
        Value::Null
    };
    let default_value = coerce_to_column_type(engine, table, column, default_value)?;
    let vector_value: Option<Vec<Vec<f32>>> = match engine
        .column_type(table, column)
        .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
    {
        Some(ty) if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) => {
            Some(index_vectors_for_type(&default_value, &ty)?)
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
    Ok(())
}

// DDL
