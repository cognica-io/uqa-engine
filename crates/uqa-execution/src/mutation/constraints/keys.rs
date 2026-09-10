//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal key validation and deterministic uniqueness reservations.
use super::{
    dml_storage_error, missing_document_error, ConstraintContext, DocId, Document,
    EnforcedKeyExecution, SQLError, TableKeyConstraint, Value,
};
use sha2::{Digest, Sha256};
use uqa_sql::semantics::period::{period_ranges, period_values_overlap};

pub fn without_overlaps_conflict(
    context: ConstraintContext<'_>,
    table: &str,
    constraint: &TableKeyConstraint,
    document: &Document,
    ignored_doc_id: Option<DocId>,
) -> Result<bool, SQLError> {
    let Some(period_column) = constraint.columns.last() else {
        return Err(SQLError::Internal(
            "WITHOUT OVERLAPS constraint has no period column".into(),
        ));
    };
    let period_type = context
        .catalog
        .column_type(table, period_column)
        .map_err(|error| dml_storage_error("WITHOUT OVERLAPS type lookup", error))?
        .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{period_column}")))?;
    let candidate_period = document.get(period_column).cloned().unwrap_or(Value::Null);
    if matches!(candidate_period, Value::Null) {
        return Ok(false);
    }
    let (_, candidate_ranges) = period_ranges(&candidate_period, &period_type)?;
    if candidate_ranges.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "23514".into(),
            message: format!(
                "empty WITHOUT OVERLAPS value found in column \"{period_column}\" in relation \"{table}\""
            ),
        });
    }
    let ordinary_columns = &constraint.columns[..constraint.columns.len() - 1];
    for doc_id in context.reads.table_doc_ids(table)? {
        if ignored_doc_id == Some(doc_id) {
            continue;
        }
        let Some(existing) = context.reads.get_document(table, doc_id)? else {
            return Err(missing_document_error(
                "WITHOUT OVERLAPS scan",
                table,
                doc_id,
            ));
        };
        if !ordinary_columns.iter().all(|column| {
            existing.get(column).cloned().unwrap_or(Value::Null)
                == document.get(column).cloned().unwrap_or(Value::Null)
        }) {
            continue;
        }
        let existing_period = existing.get(period_column).cloned().unwrap_or(Value::Null);
        if matches!(existing_period, Value::Null) {
            continue;
        }
        if period_values_overlap(&candidate_period, &existing_period, &period_type)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn lock_document_key_dependencies(
    context: ConstraintContext<'_>,
    table: &str,
    document: &Document,
    old_document: Option<&Document>,
) -> Result<Vec<crate::row_locks::RowLockAcquisition>, SQLError> {
    let canonical_table = context
        .partitions
        .catalog
        .try_resolve_table_name(table)
        .map_err(|error| dml_storage_error("key-lock table resolution", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let constraints = context
        .catalog
        .enforced_keys(&canonical_table)
        .map_err(|error| dml_storage_error("key-lock constraint lookup", error))?;
    let mut lock_keys = std::collections::BTreeSet::new();
    for constraint in constraints {
        let Some(values) = constraint.values(context, table, document)? else {
            continue;
        };
        if old_document
            .map(|old_document| constraint.values(context, table, old_document))
            .transpose()?
            .flatten()
            .as_ref()
            == Some(&values)
        {
            continue;
        }
        let lock_values = if constraint.without_overlaps {
            &values[..values.len().saturating_sub(1)]
        } else {
            values.as_slice()
        };
        let key =
            crate::canonical_row_key(lock_values).map_err(crate::physical::physical_exec_error)?;
        let mut digest = Sha256::new();
        digest.update(b"uqa-key-lock-v1");
        update_key_lock_digest(&mut digest, canonical_table.as_bytes())?;
        digest.update([match constraint.kind {
            uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => 0,
            uqa_sql::ast::TableKeyConstraintKind::Unique => 1,
        }]);
        digest.update([u8::from(constraint.nulls_not_distinct)]);
        digest.update([u8::from(constraint.without_overlaps)]);
        let identity = serde_json::to_vec(&constraint.keys)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        update_key_lock_digest(&mut digest, &identity)?;
        update_key_lock_digest(&mut digest, &key)?;
        let digest: [u8; 32] = digest.finalize().into();
        lock_keys.insert(digest);
    }

    let has_reservations = !lock_keys.is_empty();
    let mut acquisitions = Vec::new();
    for lock_key in lock_keys {
        match context.transactions.lock_key_reservation(lock_key, table)? {
            crate::row_locks::LockAcquire::Granted { acquisition, .. } => {
                acquisitions.extend(acquisition);
            }
            crate::row_locks::LockAcquire::Skipped => {
                return Err(SQLError::Internal(
                    "blocking key reservation unexpectedly skipped a key".into(),
                ));
            }
        }
    }
    if has_reservations {
        // A competing writer can publish and release this reservation after our initial snapshot but immediately before acquisition. That grant does not report a wait, so every reservation boundary must refresh the READ COMMITTED snapshot before conflict lookup.
        context.transactions.refresh_explicit_statement_snapshot()?;
    }
    Ok(acquisitions)
}

fn update_key_lock_digest(digest: &mut Sha256, part: &[u8]) -> Result<(), SQLError> {
    let len = u64::try_from(part.len())
        .map_err(|_| SQLError::Internal("key-lock digest part exceeds u64".into()))?;
    digest.update(len.to_be_bytes());
    digest.update(part);
    Ok(())
}

pub fn validate_key_constraints(
    context: ConstraintContext<'_>,
    table: &str,
    document: &Document,
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    for constraint in context
        .catalog
        .enforced_keys(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        let Some(values) = constraint.values(context, table, document)? else {
            continue;
        };
        if constraint.without_overlaps {
            if !without_overlaps_conflict(context, table, &constraint, document, ignored_doc_id)? {
                continue;
            }
            let name = constraint.name.as_deref().unwrap_or("<unnamed>");
            return Err(SQLError::Routine {
                sqlstate: "23P01".into(),
                message: format!("conflicting key value violates exclusion constraint \"{name}\""),
            });
        }
        let Some(conflict_id) =
            constraint.find_conflict(context, table, &values, ignored_doc_id)?
        else {
            continue;
        };
        if ignored_doc_id == Some(conflict_id) {
            continue;
        }
        let name = constraint.name.as_deref().unwrap_or("<unnamed>");
        return Err(SQLError::Routine {
            sqlstate: "23505".into(),
            message: format!("duplicate key value violates unique constraint \"{name}\""),
        });
    }
    Ok(())
}
