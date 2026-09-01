//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE execution.

use super::defaults::validate_default_expression;
use super::{
    ddl_storage_error, prepare_create_table_hierarchy, ColumnType, CreateTable, Engine, SQLError,
    SQLResult,
};
use crate::sql::generated::prepare_generated_columns;
use uqa_sql::ast::{AutoIncrementKind, AutoIncrementOwner, Expr, SequenceDataType};

use super::constraint_validation::{
    resolve_foreign_key_parent, validate_check_expression, validate_foreign_key_definition,
};

// -------------------------------------------------------------------------

pub(in crate::sql) fn run_create_table(
    engine: &Engine,
    c: CreateTable,
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_create_table_inner(engine, c))
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
fn run_create_table_inner(engine: &Engine, mut c: CreateTable) -> Result<SQLResult, SQLError> {
    for column in &c.columns {
        super::validate_postgres_column_name(&column.name)?;
    }
    if c.persistence != uqa_sql::ast::RelationPersistence::Temporary {
        engine.prepare_explicit_transaction_writer()?;
    }
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
    prepare_create_table_hierarchy(engine, &mut c)?;
    materialize_implicit_sequences(engine, &mut c)?;
    let check_columns = c.columns.clone();
    for column in &mut c.columns {
        if let Some(default) = &column.default {
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

fn materialize_implicit_sequences(
    engine: &Engine,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    let relation = crate::RelationIdentity::from_legacy_name(&table.name)
        .map_err(|error| SQLError::Internal(format!("resolve CREATE TABLE relation: {error}")))?;
    for column in &mut table.columns {
        let Some(auto_increment) = column.auto_increment.as_mut() else {
            continue;
        };
        if auto_increment.kind == AutoIncrementKind::Legacy || auto_increment.sequence.is_some() {
            continue;
        }
        let data_type = match &column.ty {
            ColumnType::SmallInteger => SequenceDataType::SmallInt,
            ColumnType::Integer => SequenceDataType::Integer,
            ColumnType::BigInteger => SequenceDataType::BigInt,
            _ => {
                return Err(SQLError::Internal(format!(
                    "implicit sequence column `{}` has non-integer type",
                    column.name
                )))
            }
        };
        let sequence = crate::RelationIdentity::new(
            relation.schema.clone(),
            format!("{}_{}_seq", relation.name, column.name),
        )
        .qualified_name();
        engine
            .create_sequence_with_persistence(&sequence, 1, 1, data_type, false, table.persistence)
            .map_err(|error| {
                SQLError::Unsupported(format!(
                    "CREATE TABLE implicit sequence `{sequence}`: {error}"
                ))
            })?;
        auto_increment.sequence = Some(sequence.clone());
        auto_increment.owner = Some(AutoIncrementOwner {
            table: table.name.clone(),
            column: column.name.clone(),
        });
        if auto_increment.kind == AutoIncrementKind::Serial {
            column.default = Some(Expr::Func {
                name: "nextval".into(),
                binding: None,
                args: vec![Expr::Literal(uqa_core::Value::Str(sequence))],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            });
        }
    }
    Ok(())
}
