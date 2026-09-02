//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key definition normalization and existing-row validation.

use super::super::{ddl_storage_error, Engine, SQLError};
use super::constraint_error;

pub(super) fn validate_foreign_key_definition(
    engine: &Engine,
    table: &str,
    foreign_key: &mut uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    validate_foreign_key_definition_with_local_state(engine, table, None, None, foreign_key)
}

pub(in crate::sql::ddl) fn validate_foreign_key_definition_with_local_state(
    engine: &Engine,
    table: &str,
    local_columns: Option<&[uqa_sql::ast::ColumnDef]>,
    local_keys: Option<&[uqa_sql::ast::TableKeyConstraint]>,
    foreign_key: &mut uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    foreign_key.ref_table = engine.resolve_visible_table_reference(&foreign_key.ref_table)?;
    validate_bound_foreign_key_definition_with_local_state(
        engine,
        table,
        local_columns,
        local_keys,
        foreign_key,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
pub(in crate::sql::ddl) fn validate_bound_foreign_key_definition_with_local_state(
    engine: &Engine,
    table: &str,
    local_columns: Option<&[uqa_sql::ast::ColumnDef]>,
    local_keys: Option<&[uqa_sql::ast::TableKeyConstraint]>,
    foreign_key: &mut uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    let stored_columns;
    let columns = if let Some(columns) = local_columns {
        columns
    } else {
        stored_columns = engine
            .try_describe_table(table)
            .map_err(|error| ddl_storage_error("FOREIGN KEY local table", error))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        &stored_columns
    };
    for column in &foreign_key.local_columns {
        if !columns.iter().any(|definition| definition.name == *column) {
            return Err(SQLError::UnknownColumn(format!("{table}.{column}")));
        }
    }
    let referenced = engine
        .try_resolve_bound_table_name(&foreign_key.ref_table)?
        .ok_or_else(|| SQLError::UnknownTable(foreign_key.ref_table.clone()))?;
    let referenced_columns = engine
        .try_describe_table(&referenced)
        .map_err(|error| ddl_storage_error("FOREIGN KEY referenced columns", error))?
        .ok_or_else(|| SQLError::UnknownTable(referenced.clone()))?;
    let local = engine
        .try_resolve_bound_table_name(table)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let referenced_keys = if referenced == local {
        match local_keys {
            Some(keys) => keys.to_vec(),
            None => engine
                .try_key_constraints(&referenced)
                .map_err(|error| ddl_storage_error("FOREIGN KEY referenced key", error))?,
        }
    } else {
        engine
            .try_key_constraints(&referenced)
            .map_err(|error| ddl_storage_error("FOREIGN KEY referenced key", error))?
    };
    if foreign_key.ref_columns.is_empty() {
        let primary_key = referenced_keys
            .iter()
            .find(|key| key.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey)
            .ok_or_else(|| {
                constraint_error(
                    "42704",
                    format!("there is no primary key for referenced table \"{referenced}\""),
                )
            })?;
        foreign_key.ref_columns.clone_from(&primary_key.columns);
    }
    if foreign_key.local_columns.len() != foreign_key.ref_columns.len() {
        return Err(constraint_error(
            "42830",
            "number of referencing and referenced columns for foreign key disagree",
        ));
    }
    for (local_column, referenced_column) in foreign_key
        .local_columns
        .iter()
        .zip(&foreign_key.ref_columns)
    {
        let local_definition = columns
            .iter()
            .find(|definition| definition.name == *local_column)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{local_column}")))?;
        let referenced_definition = referenced_columns
            .iter()
            .find(|definition| definition.name == *referenced_column)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{referenced}.{referenced_column}")))?;
        if uqa_execution::foreign_key_operand_type(&local_definition.ty, &referenced_definition.ty)
            .is_err()
        {
            return Err(constraint_error(
                "42804",
                format!(
                    "foreign key constraint cannot be implemented: key columns \"{local_column}\" and \"{referenced_column}\" are of incompatible types: {} and {}",
                    local_definition.ty.sql_name(),
                    referenced_definition.ty.sql_name()
                ),
            ));
        }
    }
    if foreign_key.period {
        super::super::constraint_validation::validate_foreign_key_definition(
            table,
            columns,
            &referenced,
            &referenced_columns,
            &referenced_keys,
            foreign_key,
        )?;
    } else {
        let referenced_column_set = foreign_key
            .ref_columns
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        let has_unique_key = referenced_column_set.len() == foreign_key.ref_columns.len()
            && referenced_keys.iter().any(|key| {
                key.columns.len() == foreign_key.ref_columns.len()
                    && key
                        .columns
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        == referenced_column_set
            });
        if !has_unique_key {
            return Err(constraint_error(
                "42830",
                format!(
                    "there is no unique constraint matching given keys for referenced table \"{referenced}\""
                ),
            ));
        }
    }
    foreign_key.ref_table = referenced;
    Ok(())
}

pub(in crate::sql::ddl) fn column_foreign_key(
    column: &uqa_sql::ast::ColumnDef,
    reference: &uqa_sql::ast::ForeignKeyRef,
) -> uqa_sql::ast::ForeignKey {
    uqa_sql::ast::ForeignKey {
        name: reference.name.clone(),
        object_id: reference.object_id,
        local_columns: vec![column.name.clone()],
        ref_table: reference.table.clone(),
        ref_columns: reference.column.iter().cloned().collect(),
        on_update: reference.on_update,
        on_delete: reference.on_delete,
        on_delete_set_columns: Vec::new(),
        match_type: reference.match_type,
        enforced: reference.enforced,
        validated: reference.validated,
        deferrable: reference.deferrable,
        initially_deferred: reference.initially_deferred,
        period: reference.period,
    }
}

pub(super) fn validate_foreign_key_rows(
    engine: &Engine,
    table: &str,
    name: &str,
    foreign_key: &uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    for doc_id in engine.live_table_doc_ids(table)? {
        let Some(document) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let Some(values) =
            crate::sql::dml::foreign_key_lookup_values(engine, table, foreign_key, &document)?
        else {
            continue;
        };
        let parent_exists = if foreign_key.period {
            crate::sql::dml::period_foreign_key_coverage(
                engine,
                foreign_key,
                &values.values,
                &[],
                None,
            )?
            .0
        } else {
            crate::sql::dml::find_foreign_key_parent(engine, foreign_key, &values)?.is_some()
        };
        if !parent_exists {
            return Err(foreign_key_violation(table, name));
        }
    }
    Ok(())
}

fn foreign_key_violation(table: &str, name: &str) -> SQLError {
    constraint_error(
        "23503",
        format!("insert or update on table \"{table}\" violates foreign key constraint \"{name}\""),
    )
}
