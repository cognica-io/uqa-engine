//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate newly declared constraints against existing physical rows.
use crate::mutation::constraints::context::{ConstraintContext, MutationRead};
use uqa_core::Value;
use uqa_sql::{
    assignment::columns::{AssignmentColumnCatalog, ColumnCatalogError},
    semantics::partition::PartitionExpressions,
    SQLError,
};
pub struct CheckValidationContext<'a> {
    pub columns: &'a dyn AssignmentColumnCatalog,
    pub reads: &'a dyn MutationRead,
    pub expressions: &'a dyn PartitionExpressions,
}
fn ddl_storage_error(action: &str, error: ColumnCatalogError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, error.as_ref())
}
fn constraint_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
pub fn validate_foreign_key_rows(
    context: ConstraintContext<'_>,
    table: &str,
    name: &str,
    foreign_key: &uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    for doc_id in context.reads.live_table_doc_ids(table)? {
        let Some(document) = context.reads.get_document(table, doc_id)? else {
            continue;
        };
        let Some(values) = uqa_sql::semantics::foreign_keys::foreign_key_lookup_values(
            context.partitions.catalog,
            table,
            foreign_key,
            &document,
        )?
        else {
            continue;
        };
        let parent_exists = if foreign_key.period {
            crate::mutation::constraints::period::period_foreign_key_coverage(
                context,
                foreign_key,
                &values.values,
                &[],
                None,
            )?
            .0
        } else {
            crate::mutation::constraints::find_foreign_key_parent(context, foreign_key, &values)?
                .is_some()
        };
        if !parent_exists {
            return Err(foreign_key_violation(table, name));
        }
    }
    Ok(())
}

/// Validate the foreign keys that a rewrite of `table` rebuilds because they involve one of its `changed` columns: those of `table`, against the rows they reference, and those of other tables that reference `table`, against its rewritten rows. `PostgreSQL` validates a rebuilt foreign key whatever the replication role and leaves a `NOT VALID` one unvalidated.
pub fn validate_rewritten_foreign_keys(
    context: ConstraintContext<'_>,
    table: &str,
    changed: &[String],
) -> Result<(), SQLError> {
    for foreign_key in context
        .catalog
        .try_foreign_keys(table)
        .map_err(|error| SQLError::Internal(format!("foreign key lookup: {error}")))?
    {
        if !foreign_key.enforced
            || !foreign_key.validated
            || !foreign_key
                .local_columns
                .iter()
                .any(|column| changed.contains(column))
        {
            continue;
        }
        let name = foreign_key
            .name
            .clone()
            .unwrap_or_else(|| "<unnamed>".into());
        validate_foreign_key_rows(context, table, &name, &foreign_key)?;
    }
    let mut checked = std::collections::BTreeSet::new();
    for target in context.referrers.partition_ancestor_tables(table)? {
        let referrers = context
            .referrers
            .try_referrers_to(&target)
            .map_err(|error| SQLError::Internal(format!("foreign key lookup: {error}")))?;
        for (declaring_table, foreign_key) in referrers {
            if !foreign_key.enforced
                || !foreign_key.validated
                || !foreign_key
                    .ref_columns
                    .iter()
                    .any(|column| changed.contains(column))
            {
                continue;
            }
            let name = foreign_key
                .name
                .clone()
                .unwrap_or_else(|| "<unnamed>".into());
            for physical_table in uqa_sql::semantics::partition::foreign_key_scan_tables(
                context.partitions.catalog,
                &declaring_table,
            )? {
                if checked.insert((physical_table.clone(), name.clone())) {
                    validate_foreign_key_rows(context, &physical_table, &name, &foreign_key)?;
                }
            }
        }
    }
    Ok(())
}

fn foreign_key_violation(table: &str, name: &str) -> SQLError {
    let table = uqa_sql::semantics::foreign_keys::foreign_key_relation_name(table);
    constraint_error(
        "23503",
        format!("insert or update on table \"{table}\" violates foreign key constraint \"{name}\""),
    )
}

pub fn validate_not_null_rows(
    reads: &dyn MutationRead,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    let relation = uqa_core::RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve NOT NULL relation: {error}")))?;
    for doc_id in reads.live_table_doc_ids(table)? {
        let Some(document) = reads.get_document(table, doc_id)? else {
            continue;
        };
        if matches!(document.get(column), None | Some(Value::Null)) {
            return Err(constraint_error(
                "23502",
                format!(
                    "column \"{column}\" of relation \"{}\" contains null values",
                    relation.name
                ),
            ));
        }
    }
    Ok(())
}

pub fn validate_check_rows(
    context: &CheckValidationContext<'_>,
    table: &str,
    name: &str,
    expression: &uqa_sql::ast::Expr,
) -> Result<(), SQLError> {
    let definitions = context
        .columns
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("VALIDATE CHECK", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let schema = crate::RowSchema::with_types(
        definitions
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        definitions
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    for doc_id in context.reads.live_table_doc_ids(table)? {
        let Some(mut document) = context.reads.get_document(table, doc_id)? else {
            continue;
        };
        crate::query::generated::materialize_virtual_generated_columns(
            &definitions,
            &mut document,
        )?;
        let value = context
            .expressions
            .evaluate_row(expression, &document, &schema, &[])?;
        if !matches!(value, Value::Null) && !uqa_sql::expr::truthy(&value) {
            let relation = uqa_core::RelationIdentity::from_legacy_name(table)
                .map_or_else(|_| table.to_string(), |identity| identity.name);
            return Err(constraint_error(
                "23514",
                format!(
                    "check constraint \"{name}\" of relation \"{relation}\" is violated by some row"
                ),
            ));
        }
    }
    Ok(())
}
