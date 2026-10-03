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
/// Check the key constraints of `table` across the rows a rewrite produced, as rebuilding their indexes does: a NULL in a primary key column reports the column's NOT NULL violation, and a repeated key `could not create unique index` with the repeated key.
pub fn validate_key_constraint_rows(
    context: &KeyValidationContext<'_>,
    table: &str,
    rows: &[(uqa_core::DocId, Document)],
) -> Result<(), SQLError> {
    for constraint in context
        .catalog
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("table rewrite", error))?
    {
        if constraint.without_overlaps {
            continue;
        }
        let mut seen = std::collections::BTreeSet::new();
        for (_, document) in rows {
            let values = constraint
                .columns
                .iter()
                .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>();
            if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey {
                if let Some(column) = constraint
                    .columns
                    .iter()
                    .zip(&values)
                    .find_map(|(column, value)| matches!(value, Value::Null).then_some(column))
                {
                    return Err(null_key_column(table, column));
                }
            }
            if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
                && values.iter().any(|value| matches!(value, Value::Null))
                && !constraint.nulls_not_distinct
            {
                continue;
            }
            if seen.contains(&values) {
                return Err(duplicated_key(context, table, &constraint, &values)?);
            }
            seen.insert(values);
        }
    }
    Ok(())
}

/// Check the `WITHOUT OVERLAPS` keys of `table` across its stored rows, once a rewrite has written them; their overlap is not an equality of key values.
pub fn validate_temporal_key_rows(
    context: &KeyValidationContext<'_>,
    table: &str,
) -> Result<(), SQLError> {
    for constraint in context
        .catalog
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("table rewrite", error))?
    {
        if constraint.without_overlaps {
            validate_key_index_rows(context, table, &constraint)?;
        }
    }
    Ok(())
}

/// `could not create unique index "x"` with `Key (a)=(1) is duplicated.`, for a key constraint whose index finds a repeated key.
fn duplicated_key(
    context: &KeyValidationContext<'_>,
    table: &str,
    constraint: &uqa_sql::ast::TableKeyConstraint,
    values: &[Value],
) -> Result<SQLError, SQLError> {
    let name = constraint.name.as_deref().ok_or_else(|| {
        SQLError::Internal("key validation requires its reserved index name".into())
    })?;
    Ok(SQLError::Diagnostic {
        sqlstate: "23505".into(),
        message: format!("could not create unique index \"{name}\""),
        detail: crate::mutation::constraints::duplicate_index_key_detail(
            context.constraints,
            table,
            &constraint.clone().into(),
            values,
        )?,
        hint: None,
    })
}

/// Check the rows of `table` as building a key's index does: a repeated key fails as `could not create unique index` with the index's name, and a `WITHOUT OVERLAPS` key fails on overlapping periods. A key value with a NULL never repeats unless the key treats NULLs as not distinct.
pub fn validate_key_index_rows(
    context: &KeyValidationContext<'_>,
    table: &str,
    constraint: &uqa_sql::ast::TableKeyConstraint,
) -> Result<(), SQLError> {
    let mut seen = std::collections::BTreeSet::<Vec<Value>>::new();
    for doc_id in context.constraints.reads.live_table_doc_ids(table)? {
        let Some(document) = context.constraints.reads.get_document(table, doc_id)? else {
            continue;
        };
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
        let values: Vec<Value> = constraint
            .columns
            .iter()
            .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
            .collect();
        if values.iter().any(|value| matches!(value, Value::Null)) && !constraint.nulls_not_distinct
        {
            continue;
        }
        if seen.contains(&values) {
            return Err(duplicated_key(context, table, constraint, &values)?);
        }
        seen.insert(values);
    }
    Ok(())
}

/// Check the NOT NULL constraints that a primary key gives its columns, after its index is built, as `PostgreSQL` verifies new NOT NULL constraints once the table's other changes are done: the first row holding a NULL in a key column fails on the first such column of the table.
pub fn validate_primary_key_rows(
    context: &KeyValidationContext<'_>,
    table: &str,
    constraint: &uqa_sql::ast::TableKeyConstraint,
) -> Result<(), SQLError> {
    if constraint.kind != uqa_sql::ast::TableKeyConstraintKind::PrimaryKey {
        return Ok(());
    }
    let columns = context
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .into_iter()
        .map(|column| column.name)
        .filter(|column| constraint.columns.contains(column))
        .collect::<Vec<_>>();
    for doc_id in context.constraints.reads.live_table_doc_ids(table)? {
        let Some(document) = context.constraints.reads.get_document(table, doc_id)? else {
            continue;
        };
        if let Some(column) = columns.iter().find(|column| {
            matches!(
                document.get(column.as_str()).unwrap_or(&Value::Null),
                Value::Null
            )
        }) {
            return Err(null_key_column(table, column));
        }
    }
    Ok(())
}

/// `column "a" of relation "t" contains null values`, for a NOT NULL constraint that a row violates.
fn null_key_column(table: &str, column: &str) -> SQLError {
    let relation = uqa_core::RelationIdentity::from_legacy_name(table)
        .map_or_else(|_| table.to_string(), |identity| identity.name);
    SQLError::Routine {
        sqlstate: "23502".into(),
        message: format!("column \"{column}\" of relation \"{relation}\" contains null values"),
    }
}
