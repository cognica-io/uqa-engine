//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Verify newly declared keys and generated replacement rows against visible physical data.
use super::hierarchy::HierarchyCatalog;
use crate::mutation::constraints::context::ConstraintContext;
use uqa_core::Value;
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;
pub struct KeyValidationContext<'a> {
    pub catalog: &'a dyn HierarchyCatalog,
    pub constraints: ConstraintContext<'a>,
}
fn ddl_storage_error(action: &str, error: uqa_storage::StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}
pub fn validate_key_constraint_rows(
    context: &KeyValidationContext<'_>,
    table: &str,
    rows: &[(uqa_core::DocId, Document)],
) -> Result<(), SQLError> {
    for constraint in context
        .catalog
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

pub fn validate_added_key_constraint(
    context: &KeyValidationContext<'_>,
    table: &str,
    constraint: &uqa_sql::ast::TableKeyConstraint,
) -> Result<(), SQLError> {
    validate_added_key_declaration(context, table, constraint)?;
    let mut seen = std::collections::BTreeSet::<Vec<Value>>::new();
    for doc_id in context.constraints.reads.live_table_doc_ids(table)? {
        let Some(document) = context.constraints.reads.get_document(table, doc_id)? else {
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
            if crate::mutation::constraints::without_overlaps_conflict(
                context.constraints,
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

fn validate_added_key_declaration(
    context: &KeyValidationContext<'_>,
    table: &str,
    constraint: &uqa_sql::ast::TableKeyConstraint,
) -> Result<(), SQLError> {
    let columns = context
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    uqa_sql::schema::keys::validate_added_key_columns(table, constraint, &columns)?;

    let existing_keys = context
        .catalog
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
    let (checks, foreign_keys) = if constraint.name.is_some() {
        let checks = context
            .catalog
            .try_check_constraint_definitions(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
        let foreign_keys = context
            .catalog
            .try_foreign_keys(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
        (checks, foreign_keys)
    } else {
        (Vec::new(), Vec::new())
    };
    uqa_sql::schema::keys::validate_added_key_identity(
        table,
        constraint,
        &existing_keys,
        &checks,
        &foreign_keys,
    )?;

    Ok(())
}
