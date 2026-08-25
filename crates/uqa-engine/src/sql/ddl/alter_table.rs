//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER TABLE schema mutation and existing-row backfill.

use super::{
    coerce_to_column_type, ddl_storage_error, eval_lowered_expression, index_vectors_for_type,
    rewrite_column_values_to_type, AlterTableAction, AlterTableStmt, BTreeMap, ColumnType,
    Document, Engine, RowUpdateVectors, SQLError, SQLResult, Value,
};
use uqa_sql::ast::{ForeignKey, GeneratedColumn, GeneratedColumnKind};

use super::constraint_validation::{resolve_foreign_key_parent, validate_foreign_key_definition};
use super::defaults::validate_default_expression;

pub(in crate::sql) fn run_alter_table(
    engine: &Engine,
    mut stmt: AlterTableStmt,
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
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::AccessExclusive,
    )?;
    engine.with_implicit_transaction(move |engine| run_alter_table_inner(engine, stmt))
}

fn run_alter_table_inner(engine: &Engine, stmt: AlterTableStmt) -> Result<SQLResult, SQLError> {
    match stmt.action {
        AlterTableAction::AddColumn {
            mut column,
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
            if let Some(default) = &column.default {
                validate_default_expression(engine, default)?;
            }
            let mut candidate_columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            candidate_columns.push(column.clone());
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut candidate_columns,
                &key_constraints,
                &foreign_keys,
            )?;
            column.generated = candidate_columns
                .last()
                .and_then(|candidate| candidate.generated.clone());
            let generated_kind = column.generated.as_ref().map(|generated| generated.kind);
            match column.ty {
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .create_vector_field(&stmt.table, col_name.clone(), dim)
                        .map_err(|err| ddl_storage_error("ALTER TABLE vector field", err))?;
                }
                ColumnType::Text if generated_kind != Some(GeneratedColumnKind::Virtual) => {
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
            if let Some(kind) = generated_kind {
                validate_and_rewrite_generated_rows(
                    engine,
                    &stmt.table,
                    kind == GeneratedColumnKind::Stored,
                )?;
            } else {
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
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
        }
        AlterTableAction::AddKeyConstraint { constraint } => {
            let mut columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let mut key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            key_constraints.push(constraint.clone());
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut columns,
                &key_constraints,
                &foreign_keys,
            )?;
            validate_added_key_constraint(engine, &stmt.table, &constraint)?;
            engine
                .add_key_constraint(&stmt.table, &constraint)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
        }
        AlterTableAction::AddForeignKey { mut foreign_key } => {
            let mut columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            let mut foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            validate_added_constraint_name(
                engine,
                &stmt.table,
                foreign_key.name.as_deref(),
                &key_constraints,
                &foreign_keys,
            )?;
            let (canonical_parent, parent_columns, parent_keys) =
                resolve_foreign_key_parent(engine, &foreign_key.ref_table)?;
            validate_foreign_key_definition(
                &stmt.table,
                &columns,
                &canonical_parent,
                &parent_columns,
                &parent_keys,
                &foreign_key,
            )?;
            foreign_key.ref_table = canonical_parent;
            validate_existing_foreign_key_rows(engine, &stmt.table, &foreign_key)?;
            foreign_keys.push(foreign_key.clone());
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut columns,
                &key_constraints,
                &foreign_keys,
            )?;
            engine
                .add_foreign_key(&stmt.table, &foreign_key)
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
            reject_default_change_on_generated_column(engine, &stmt.table, &name)?;
            validate_default_expression(engine, &default)?;
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
            reject_default_change_on_generated_column(engine, &stmt.table, &name)?;
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
        AlterTableAction::SetExpression { name, expression } => {
            let mut columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let column = columns
                .iter_mut()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let Some(current) = column.generated.as_ref() else {
                return Err(SQLError::TypeMismatch(format!(
                    "column `{name}` of relation `{}` is not a generated column",
                    stmt.table
                )));
            };
            let kind = current.kind;
            let generated = GeneratedColumn {
                kind,
                expression: Box::new(expression),
                function_dependencies: Vec::new(),
            };
            column.generated = Some(generated.clone());
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut columns,
                &key_constraints,
                &foreign_keys,
            )?;
            let generated = columns
                .iter()
                .find(|column| column.name == name)
                .and_then(|column| column.generated.clone())
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "generated column `{name}` disappeared during validation"
                    ))
                })?;
            engine
                .set_column_generated(&stmt.table, &name, Some(generated))
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            validate_and_rewrite_generated_rows(
                engine,
                &stmt.table,
                kind == GeneratedColumnKind::Stored,
            )?;
        }
        AlterTableAction::DropExpression { name } => {
            let columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN DROP EXPRESSION", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let column = columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let Some(generated) = column.generated.as_ref() else {
                return Err(SQLError::TypeMismatch(format!(
                    "column `{name}` of relation `{}` is not a generated column",
                    stmt.table
                )));
            };
            if generated.kind == GeneratedColumnKind::Virtual {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE / DROP EXPRESSION is not supported for virtual generated column `{name}`"
                )));
            }
            engine
                .set_column_generated(&stmt.table, &name, None)
                .map_err(|error| ddl_storage_error("ALTER COLUMN DROP EXPRESSION", error))?;
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
        AlterTableAction::AlterColumnType { name, ty, using } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            let mut candidate_columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let candidate = candidate_columns
                .iter_mut()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            candidate.ty.clone_from(&ty);
            let target_generated_kind =
                candidate.generated.as_ref().map(|generated| generated.kind);
            if target_generated_kind.is_none() {
                let dependents = engine
                    .generated_columns_referencing_column(&stmt.table, &name)
                    .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
                if !dependents.is_empty() {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot alter type of column `{name}` because generated column(s) `{}` depend on it",
                        dependents.join("`, `")
                    )));
                }
            }
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
            validate_altered_constraint_column_types(
                engine,
                &stmt.table,
                &candidate_columns,
                &key_constraints,
                &foreign_keys,
            )?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut candidate_columns,
                &key_constraints,
                &foreign_keys,
            )?;
            let old_ty = engine
                .column_type(&stmt.table, &name)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let old_was_vector = matches!(&old_ty, ColumnType::Vector(_) | ColumnType::Tensor(_));
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
            if target_generated_kind.is_none() {
                rewrite_column_values_to_type(
                    engine,
                    &stmt.table,
                    &name,
                    &old_ty,
                    &ty,
                    using.as_ref(),
                )?;
            }
            engine
                .set_column_type(&stmt.table, &name, &ty)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?;
            match ty {
                ColumnType::Text if target_generated_kind != Some(GeneratedColumnKind::Virtual) => {
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
            if let Some(kind) = target_generated_kind {
                validate_and_rewrite_generated_rows(
                    engine,
                    &stmt.table,
                    kind == GeneratedColumnKind::Stored,
                )?;
            }
            validate_all_table_rows(engine)?;
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
    }
    Ok(SQLResult::empty())
}

fn validate_altered_constraint_column_types(
    engine: &Engine,
    table: &str,
    candidate_columns: &[uqa_sql::ast::ColumnDef],
    key_constraints: &[uqa_sql::ast::TableKeyConstraint],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    for constraint in key_constraints
        .iter()
        .filter(|constraint| constraint.without_overlaps)
    {
        let Some(period_column) = constraint.columns.last() else {
            return Err(SQLError::Internal(
                "WITHOUT OVERLAPS constraint has no period column".into(),
            ));
        };
        let period_type = candidate_columns
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
    }

    for foreign_key in foreign_keys.iter().filter(|foreign_key| foreign_key.period) {
        let (parent_name, parent_columns, parent_keys) =
            resolve_foreign_key_parent(engine, &foreign_key.ref_table)?;
        let parent_columns = if parent_name == table {
            candidate_columns
        } else {
            parent_columns.as_slice()
        };
        validate_foreign_key_definition(
            table,
            candidate_columns,
            &parent_name,
            parent_columns,
            &parent_keys,
            foreign_key,
        )?;
    }

    for (child_table, foreign_key) in engine
        .try_referrers_to(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        .into_iter()
        .filter(|(_, foreign_key)| foreign_key.period)
    {
        let child_columns = if child_table == table {
            candidate_columns.to_vec()
        } else {
            engine
                .try_describe_table(&child_table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
                .ok_or_else(|| SQLError::UnknownTable(child_table.clone()))?
        };
        validate_foreign_key_definition(
            &child_table,
            &child_columns,
            table,
            candidate_columns,
            key_constraints,
            &foreign_key,
        )?;
    }
    Ok(())
}

fn reject_default_change_on_generated_column(
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

fn validate_and_rewrite_generated_rows(
    engine: &Engine,
    table: &str,
    rewrite_physical_rows: bool,
) -> Result<(), SQLError> {
    let doc_ids = engine.table_doc_ids(table)?;
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

fn validate_key_constraint_rows(
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

fn validate_all_table_rows(engine: &Engine) -> Result<(), SQLError> {
    for table in engine
        .table_names()
        .map_err(|error| ddl_storage_error("generated-column validation", error))?
    {
        for doc_id in engine.table_doc_ids(&table)? {
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
    if constraint.without_overlaps {
        if constraint.columns.len() < 2 {
            return Err(SQLError::TypeMismatch(
                "constraint using WITHOUT OVERLAPS needs at least two columns".into(),
            ));
        }
        let period_column = constraint.columns.last().expect("validated non-empty key");
        let period_type = columns
            .iter()
            .find(|column| column.name == *period_column)
            .map(|column| &column.ty)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{period_column}")))?;
        if !matches!(
            period_type,
            ColumnType::Range(_) | ColumnType::Multirange(_)
        ) {
            return Err(SQLError::TypeMismatch(format!(
                "column `{period_column}` in WITHOUT OVERLAPS is not a range or multirange type"
            )));
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

fn validate_added_constraint_name(
    engine: &Engine,
    table: &str,
    name: Option<&str>,
    key_constraints: &[uqa_sql::ast::TableKeyConstraint],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    let Some(name) = name else {
        return Ok(());
    };
    let check_name_exists = engine
        .try_check_constraint_definitions(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
        .iter()
        .any(|existing| existing.name.as_deref() == Some(name));
    let key_name_exists = key_constraints
        .iter()
        .any(|existing| existing.name.as_deref() == Some(name));
    let foreign_name_exists = foreign_keys
        .iter()
        .any(|existing| existing.name.as_deref() == Some(name));
    if check_name_exists || key_name_exists || foreign_name_exists {
        return Err(SQLError::TypeMismatch(format!(
            "constraint `{name}` already exists on table `{table}`"
        )));
    }
    Ok(())
}

fn validate_existing_foreign_key_rows(
    engine: &Engine,
    table: &str,
    foreign_key: &ForeignKey,
) -> Result<(), SQLError> {
    if !foreign_key.enforced {
        return Ok(());
    }
    for doc_id in engine.table_doc_ids(table)? {
        let Some(document) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let Some(values) = crate::sql::dml::foreign_key_lookup_values(foreign_key, &document)?
        else {
            continue;
        };
        let covered = if foreign_key.period {
            crate::sql::dml::period_foreign_key_coverage(engine, foreign_key, &values, &[], None)?.0
        } else {
            engine
                .find_conflict(&foreign_key.ref_table, &foreign_key.ref_columns, &values)?
                .is_some()
        };
        if !covered {
            return Err(SQLError::Routine {
                sqlstate: "23503".into(),
                message: format!(
                    "insert or update on table \"{table}\" violates foreign key constraint \"{}\"",
                    foreign_key.name.as_deref().unwrap_or("<unnamed>")
                ),
            });
        }
    }
    Ok(())
}

/// Apply the new column's DEFAULT (or NULL) value to every row that
/// existed before the ADD COLUMN. `PostgreSQL` evaluates the default
/// once per existing row at ALTER TABLE time so NOT NULL columns stay
/// consistent on non-empty tables; this function preserves that semantic by
/// sweeping the document store.
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
