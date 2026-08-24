//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE execution.

use super::defaults::validate_default_expression;
use super::{ddl_storage_error, ColumnType, CreateTable, Engine, SQLError, SQLResult};
use crate::sql::generated::prepare_generated_columns;

// -------------------------------------------------------------------------

pub(in crate::sql) fn run_create_table(
    engine: &Engine,
    c: CreateTable,
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_create_table_inner(engine, c))
}

fn run_create_table_inner(engine: &Engine, mut c: CreateTable) -> Result<SQLResult, SQLError> {
    c.name = if c.persistence == uqa_sql::ast::RelationPersistence::Temporary {
        engine
            .try_temporary_relation_name_for_create(&c.name)
            .map_err(SQLError::Unsupported)?
    } else {
        engine
            .try_relation_name_for_create(&c.name)
            .map_err(SQLError::Unsupported)?
    };
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
        .create_table_with_lifecycle(
            &c.name,
            uqa_analysis::analyzer::standard_analyzer("english"),
            Vec::new(),
            c.persistence,
            c.on_commit,
        )
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
    engine
        .register_table_constraints(
            &c.name,
            c.checks.clone(),
            c.foreign_keys.clone(),
            c.key_constraints.clone(),
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
