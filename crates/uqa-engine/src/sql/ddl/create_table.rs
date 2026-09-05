//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE execution.

use super::constraint_validation::{
    resolve_foreign_key_parent, validate_check_expression, validate_foreign_key_definition,
};
use super::defaults::validate_default_expression;
use super::{
    ddl_storage_error, prepare_create_table_hierarchy, ColumnType, CreateTable, Engine, SQLError,
    SQLResult,
};
use crate::sql::generated::prepare_generated_columns;

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

fn validate_create_table_columns(table: &CreateTable) -> Result<(), SQLError> {
    for column in &table.columns {
        super::validate_postgres_column_name(&column.name)?;
        super::validate_postgres_relation_column_type(&column.name, &column.ty)?;
    }
    Ok(())
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
        crate::engine_capabilities::RelationResolution::Found(_, _)
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

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
fn create_table_after_preflight(
    engine: &Engine,
    mut c: CreateTable,
) -> Result<SQLResult, SQLError> {
    prepare_create_table_hierarchy(engine, &mut c)?;
    super::constraint_indexes::name_constraint_indexes(engine, &c.name, &mut c.key_constraints)?;
    bind_create_table_relation_references(engine, &mut c)?;
    engine.materialize_implicit_sequences(
        "CREATE TABLE",
        &c.name,
        &mut c.columns,
        c.persistence,
    )?;
    let check_columns = c.columns.clone();
    for column in &mut c.columns {
        if let Some(default) = &mut column.default {
            validate_default_expression(engine, default, &column.ty)?;
        }
        if let Some(check) = &mut column.check {
            validate_check_expression(engine, &c.name, &c.qualifier, &check_columns, check)?;
            crate::sql::reject_stored_regrole_constants(engine, check, None)?;
        }
    }
    for check in &mut c.checks {
        validate_check_expression(
            engine,
            &c.name,
            &c.qualifier,
            &check_columns,
            &mut check.expr,
        )?;
        crate::sql::reject_stored_regrole_constants(engine, &check.expr, None)?;
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
    for column in &mut registered_columns {
        let Some(reference) = column.references.clone() else {
            continue;
        };
        let mut foreign_key = super::alter_table::column_foreign_key(column, &reference);
        super::alter_table::validate_bound_foreign_key_definition_with_local_state(
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
        reference.referenced_key = foreign_key.referenced_key;
        reference.table = foreign_key.ref_table;
        reference.column = Some(referenced_column.clone());
    }
    for foreign_key in &mut c.foreign_keys {
        super::alter_table::validate_bound_foreign_key_definition_with_local_state(
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

fn bind_create_table_relation_references(
    engine: &Engine,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    let table_name = table.name.clone();
    let qualifier = table.qualifier.clone();
    for column in &mut table.columns {
        if let Some(reference) = column.references.as_mut() {
            bind_create_table_reference(engine, &table_name, &qualifier, &mut reference.table)?;
        }
    }
    for foreign_key in &mut table.foreign_keys {
        bind_create_table_reference(engine, &table_name, &qualifier, &mut foreign_key.ref_table)?;
    }
    Ok(())
}

fn bind_create_table_reference(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    reference: &mut String,
) -> Result<(), SQLError> {
    let self_reference = reference == table
        || reference == qualifier
        || table
            .rsplit_once('.')
            .is_some_and(|(_, local_name)| local_name == reference);
    if self_reference {
        table.clone_into(reference);
        return Ok(());
    }
    *reference = engine.resolve_visible_table_reference(reference)?;
    Ok(())
}
