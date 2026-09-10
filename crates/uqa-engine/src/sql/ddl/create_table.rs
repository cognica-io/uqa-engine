//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE execution.

use super::{ddl_storage_error, ColumnType, CreateTable, Engine, SQLError, SQLResult};
use uqa_sql::schema::table_creation::validate_create_table_columns;

// -------------------------------------------------------------------------

pub(in crate::sql) fn run_create_table(
    engine: &Engine,
    c: CreateTable,
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_create_table_inner(engine, c))
}

pub(in crate::sql) fn run_create_table_if_not_exists(
    engine: &Engine,
    deferred: uqa_sql::ast::DeferredCreateTable,
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| {
        let Some(name) =
            preflight_create_table_target(engine, &deferred.name, deferred.persistence, true)?
        else {
            return Ok(SQLResult::empty());
        };
        let mut table = uqa_sql::resolve_deferred_create_table(&deferred)?;
        validate_create_table_columns(&table)?;
        table.name = name;
        create_table_after_preflight(engine, table)
    })
}

fn preflight_create_table_target(
    engine: &Engine,
    name: &str,
    persistence: uqa_sql::ast::RelationPersistence,
    if_not_exists: bool,
) -> Result<Option<String>, SQLError> {
    if persistence != uqa_sql::ast::RelationPersistence::Temporary {
        engine.prepare_explicit_transaction_writer()?;
    }
    let name = if persistence == uqa_sql::ast::RelationPersistence::Temporary {
        engine.try_temporary_relation_name_for_create(name)?
    } else {
        engine.try_relation_name_for_sql_create(name)?
    };
    if matches!(
        engine.resolve_bound_relation_kind(&name)?,
        crate::capabilities::RelationResolution::Found(_, _)
    ) {
        let local = crate::RelationIdentity::from_legacy_name(&name)
            .map_err(SQLError::Internal)?
            .name;
        if if_not_exists {
            engine.push_sql_notice(
                "NOTICE",
                &format!("relation \"{local}\" already exists, skipping"),
            );
            return Ok(None);
        }
        return Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{local}\" already exists"),
        });
    }
    Ok(Some(name))
}

fn run_create_table_inner(engine: &Engine, mut c: CreateTable) -> Result<SQLResult, SQLError> {
    validate_create_table_columns(&c)?;
    let Some(name) =
        preflight_create_table_target(engine, &c.name, c.persistence, c.if_not_exists)?
    else {
        return Ok(SQLResult::empty());
    };
    c.name = name;
    create_table_after_preflight(engine, c)
}

fn create_table_after_preflight(
    engine: &Engine,
    mut c: CreateTable,
) -> Result<SQLResult, SQLError> {
    let analysis = engine.table_declaration_context();
    uqa_sql::schema::table_creation::declaration::prepare_create_table_declaration(
        &analysis, &mut c,
    )?;
    engine.materialize_implicit_sequences(
        "CREATE TABLE",
        &c.name,
        &mut c.columns,
        c.persistence,
    )?;
    uqa_sql::schema::table_creation::declaration::validate_create_table_expressions(
        &analysis, &mut c,
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
            .try_register_column_with_check_columns(&c.name, col.clone(), &c.columns)
            .map_err(|e| ddl_storage_error("CREATE TABLE column", e))?;
    }
    let mut registered_columns = engine
        .try_describe_table(&c.name)
        .map_err(|err| ddl_storage_error("CREATE TABLE columns", err))?
        .ok_or_else(|| SQLError::UnknownTable(c.name.clone()))?;
    uqa_sql::schema::table_creation::declaration::bind_created_table_foreign_keys(
        &analysis.foreign_keys,
        &mut c,
        &mut registered_columns,
    )?;
    engine
        .replace_constraint_state(
            &c.name,
            registered_columns,
            uqa_sql::ast::TableConstraintSet {
                columns_declared: Some(true),
                persistence: c.persistence,
                on_commit: c.on_commit,
                checks: c.checks.clone(),
                foreign_keys: c.foreign_keys.clone(),
                key_constraints: c.key_constraints.clone(),
                hierarchy: c.hierarchy.clone(),
            },
        )
        .map_err(|err| ddl_storage_error("CREATE TABLE constraints", err))?;
    engine
        .attach_implicit_sequence_owners(&c.name)
        .map_err(|err| ddl_storage_error("CREATE TABLE sequence ownership", err))?;
    engine
        .install_table_hierarchy(&c.name, c.hierarchy.clone())
        .map_err(|err| ddl_storage_error("CREATE TABLE hierarchy", err))?;
    engine
        .try_persist_table_schema(&c.name)
        .map_err(|e| ddl_storage_error("CREATE TABLE", e))?;
    engine
        .refresh_value_indexes_for_table(&c.name)
        .map_err(|e| ddl_storage_error("CREATE TABLE btree indexes", e))?;
    Ok(SQLResult::empty())
}
