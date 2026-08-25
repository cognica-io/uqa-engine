//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE execution.

use super::defaults::validate_default_expression;
use super::{ddl_storage_error, ColumnType, CreateTable, Engine, SQLError, SQLResult};
use crate::sql::generated::prepare_generated_columns;

use super::constraint_validation::{resolve_foreign_key_parent, validate_foreign_key_definition};

// -------------------------------------------------------------------------

pub(in crate::sql) fn run_create_table(
    engine: &Engine,
    c: CreateTable,
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_create_table_inner(engine, c))
}

fn run_create_table_inner(engine: &Engine, mut c: CreateTable) -> Result<SQLResult, SQLError> {
    if engine
        .try_has_table(&c.name)
        .map_err(|err| ddl_storage_error("CREATE TABLE", err))?
    {
        if c.if_not_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "CREATE TABLE: relation `{}` already exists",
            c.name
        )));
    }
    for column in &c.columns {
        if let Some(default) = &column.default {
            validate_default_expression(engine, default)?;
        }
    }
    for foreign_key in &mut c.foreign_keys {
        if !foreign_key.period {
            continue;
        }
        let self_reference = foreign_key.ref_table == c.name
            || foreign_key.ref_table == c.qualifier
            || c.name
                .rsplit_once('.')
                .is_some_and(|(_, local_name)| local_name == foreign_key.ref_table);
        if self_reference {
            validate_foreign_key_definition(
                &c.name,
                &c.columns,
                &c.name,
                &c.columns,
                &c.key_constraints,
                foreign_key,
            )?;
            foreign_key.ref_table.clone_from(&c.name);
        } else {
            let (canonical, parent_columns, parent_keys) =
                resolve_foreign_key_parent(engine, &foreign_key.ref_table)?;
            validate_foreign_key_definition(
                &c.name,
                &c.columns,
                &canonical,
                &parent_columns,
                &parent_keys,
                foreign_key,
            )?;
            foreign_key.ref_table = canonical;
        }
    }
    prepare_generated_columns(
        engine,
        &c.qualifier,
        &mut c.columns,
        &c.key_constraints,
        &c.foreign_keys,
    )?;
    let mut vector_fields: Vec<(String, u32)> = Vec::new();
    for col in &c.columns {
        match &col.ty {
            ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                vector_fields.push((col.name.clone(), *dim));
            }
            _ => {}
        }
    }
    engine
        .create_default_table(c.name.clone(), Vec::new())
        .map_err(|err| ddl_storage_error("CREATE TABLE", err))?;
    for (field, dim) in vector_fields {
        engine
            .create_vector_field(&c.name, field, dim)
            .map_err(|err| ddl_storage_error("CREATE TABLE vector field", err))?;
    }
    for col in &c.columns {
        engine
            .try_register_column(&c.name, col.clone())
            .map_err(|e| ddl_storage_error("CREATE TABLE column", e))?;
    }
    let mut registered_columns = engine
        .try_describe_table(&c.name)
        .map_err(|err| ddl_storage_error("CREATE TABLE columns", err))?
        .ok_or_else(|| SQLError::UnknownTable(c.name.clone()))?;
    for column in &mut registered_columns {
        let Some(reference) = column.references.clone() else {
            continue;
        };
        let mut foreign_key = super::alter_table::column_foreign_key(column, &reference);
        super::alter_table::validate_foreign_key_definition_with_local_state(
            engine,
            &c.name,
            None,
            Some(&c.key_constraints),
            &mut foreign_key,
        )?;
        let [referenced_column] = foreign_key.ref_columns.as_slice() else {
            return Err(SQLError::Internal(
                "column FOREIGN KEY did not resolve exactly one referenced column".into(),
            ));
        };
        let Some(reference) = column.references.as_mut() else {
            return Err(SQLError::Internal(
                "column FOREIGN KEY disappeared during validation".into(),
            ));
        };
        reference.table = foreign_key.ref_table;
        reference.column = Some(referenced_column.clone());
    }
    for foreign_key in &mut c.foreign_keys {
        super::alter_table::validate_foreign_key_definition_with_local_state(
            engine,
            &c.name,
            None,
            Some(&c.key_constraints),
            foreign_key,
        )?;
    }
    engine
        .replace_constraint_state(
            &c.name,
            registered_columns,
            uqa_sql::ast::TableConstraintSet {
                checks: c.checks.clone(),
                foreign_keys: c.foreign_keys.clone(),
                key_constraints: c.key_constraints.clone(),
            },
        )
        .map_err(|err| ddl_storage_error("CREATE TABLE constraints", err))?;
    engine
        .try_persist_table_schema(&c.name)
        .map_err(|e| ddl_storage_error("CREATE TABLE", e))?;
    engine
        .refresh_value_indexes_for_table(&c.name)
        .map_err(|e| ddl_storage_error("CREATE TABLE btree indexes", e))?;
    Ok(SQLResult::empty())
}
