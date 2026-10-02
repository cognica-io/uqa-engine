//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical document identity selection and sequence allocation services.
use super::{
    constraints::context::ConstraintCatalog,
    errors::{dml_storage_error, identifier_storage_error},
};
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;
mod reservation;
pub use reservation::reserve_document_id;

pub trait MutationIdentifiers {
    fn allocate_next_id(&self, table: &str) -> Result<DocId, SQLError>;
    fn advance_next_id(&self, table: &str, doc_id: DocId) -> uqa_storage::StorageBackendResult<()>;
    fn persist_next_id(&self, table: &str) -> uqa_storage::StorageBackendResult<()>;
    /// Whether every identity `allocate_next_id` returns for `table` is one no document of the table ever had. That holds where the table reserves identities in its own durable namespace, whose watermark every write of the table raises. A table that draws from another table's namespace, as a partition does, or keeps only a local counter cannot say.
    fn generates_unused_identities(&self, table: &str) -> Result<bool, SQLError>;
}
pub fn integer_primary_key_doc_id(
    catalog: &dyn ConstraintCatalog,
    table: &str,
    doc: &Document,
) -> Result<Option<DocId>, SQLError> {
    let Some(cols) = catalog
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("UPDATE primary key", err))?
    else {
        return Ok(None);
    };
    let Some(pk) = cols.iter().find(|c| c.primary_key && c.ty.is_integer()) else {
        return Ok(None);
    };
    Ok(match doc.get(&pk.name) {
        Some(Value::Int(v)) if *v >= 0 => Some(*v as DocId),
        _ => None,
    })
}

use crate::query::locking::context::QueryRowLockSession;
use uqa_sql::{
    assignment::{columns::AssignmentColumnCatalog, AssignmentContext},
    ast::AutoIncrement,
    semantics::{doc_id_value, partition::PartitionCatalog, returning::document_supplied_id},
};
pub trait InsertIdentityCatalog {
    fn auto_increment_column(&self, table: &str) -> Result<Option<String>, String>;
    fn auto_increment_columns(&self, table: &str) -> Result<Vec<(String, AutoIncrement)>, String>;
}
pub trait IdentitySequences {
    fn nextval_sql(&self, sequence: &str) -> Result<i64, SQLError>;
}
#[derive(Clone, Copy)]
pub struct InsertIdentityContext<'a> {
    pub catalog: &'a dyn InsertIdentityCatalog,
    pub columns: &'a dyn AssignmentColumnCatalog,
    pub assignment: &'a dyn AssignmentContext,
    pub identifiers: &'a dyn MutationIdentifiers,
    pub sequences: &'a dyn IdentitySequences,
    pub locks: &'a dyn QueryRowLockSession,
    pub partitions: &'a dyn PartitionCatalog,
}
pub fn insert_identity_columns(
    context: InsertIdentityContext<'_>,
    table: &str,
    action: &str,
) -> Result<(Option<String>, String, bool), SQLError> {
    let auto_increment = context
        .catalog
        .auto_increment_column(table)
        .map_err(|err| dml_storage_error(action, err))?;
    let definitions = context
        .columns
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let primary_keys = definitions
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let primary_key = (primary_keys.len() == 1).then(|| primary_keys[0].clone());
    let accepts_supplied_identity = primary_key.is_some() || definitions.is_empty();
    let id_column = primary_key.unwrap_or_else(|| "id".into());
    // Only a key names a row. A SERIAL or identity column that is not the
    // table's single primary key is an ordinary column with a sequence
    // default, and two rows may supply the same value to it, so its value
    // cannot choose the physical document. The conventional `id` field is a
    // storage identity only for the legacy schema-less document API. A
    // declared SQL table without a primary key may contain duplicate ordinary
    // `id` values, so using that field as the physical document key would
    // silently replace rows.
    Ok((auto_increment, id_column, accepts_supplied_identity))
}

/// Draw the values an inserted row leaves to its identity columns from their sequences, and select the identity of a row that a legacy counter column decides. A sequence value is an ordinary column value: when its column is the table's single primary key it names the row as any supplied key value does, so the caller selects the identity of every row this returns none for.
pub fn prepare_auto_increment_identity(
    context: InsertIdentityContext<'_>,
    table: &str,
    id_column: &str,
    accepts_supplied_identity: bool,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<Option<(DocId, bool)>, SQLError> {
    let Some(auto_id_column) = auto_id_column else {
        return Ok(None);
    };
    let definitions = context
        .catalog
        .auto_increment_columns(table)
        .map_err(|error| dml_storage_error(action, error))?;
    for (column, provenance) in &definitions {
        if !provenance.is_identity() {
            continue;
        }
        let supplied = document
            .get(column)
            .is_some_and(|value| !matches!(value, Value::Null));
        if supplied {
            if provenance.kind == uqa_sql::ast::AutoIncrementKind::IdentityAlways {
                return Err(SQLError::Routine {
                    sqlstate: "428C9".into(),
                    message: format!(
                        "cannot insert a non-DEFAULT value into identity column \"{column}\""
                    ),
                });
            }
            continue;
        }
        let sequence = provenance.sequence.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "identity column `{table}.{column}` has no durable sequence binding"
            ))
        })?;
        let value = context.sequences.nextval_sql(sequence)?;
        document.insert(
            column.clone(),
            uqa_sql::assignment::columns::coerce_to_column_type(
                context.assignment,
                context.columns,
                table,
                column,
                Value::Int(value),
            )?,
        );
    }
    let provenance = definitions
        .iter()
        .find(|(column, _)| column == auto_id_column)
        .map(|(_, provenance)| provenance)
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "auto-increment column `{table}.{auto_id_column}` disappeared"
            ))
        })?;
    if provenance.kind != uqa_sql::ast::AutoIncrementKind::Legacy {
        return Ok(None);
    }
    let owner = uqa_sql::semantics::partition::partition_identity_owner(context.partitions, table)?;
    context
        .locks
        .lock_relation(&owner, crate::row_locks::RelationLockMode::RowExclusive)?;
    if accepts_supplied_identity && auto_id_column == id_column {
        return prepare_insert_identity(
            context,
            &owner,
            id_column,
            true,
            Some(auto_id_column),
            document,
            action,
        )
        .map(Some);
    }
    let generated =
        prepare_legacy_counter_value(context, &owner, auto_id_column, document, action)?;
    // A table without a key names its rows by generated identities from the same counter, so a generated counter value names its row too and the counter advances once per row.
    Ok(generated
        .filter(|_| !accepts_supplied_identity)
        .map(|value| (value, false)))
}

/// Draw a missing value of a legacy counter column that is not the row's key from the table counter, which a supplied value advances past, and return the value drawn.
fn prepare_legacy_counter_value(
    context: InsertIdentityContext<'_>,
    owner: &str,
    column: &str,
    document: &mut Document,
    action: &str,
) -> Result<Option<DocId>, SQLError> {
    match document.get(column) {
        None | Some(Value::Null) => {
            let value = context.identifiers.allocate_next_id(owner)?;
            document.insert(column.to_string(), doc_id_value(value)?);
            Ok(Some(value))
        }
        Some(Value::Int(value)) => {
            if let Ok(value) = DocId::try_from(*value) {
                context
                    .identifiers
                    .advance_next_id(owner, value)
                    .map_err(|error| identifier_storage_error(action, &error))?;
            }
            Ok(None)
        }
        Some(_) => Ok(None),
    }
}

pub fn persist_auto_increment_identity(
    context: InsertIdentityContext<'_>,
    table: &str,
    auto_id_column: Option<&str>,
    action: &str,
) -> Result<(), SQLError> {
    let Some(auto_id_column) = auto_id_column else {
        return Ok(());
    };
    let legacy = context
        .catalog
        .auto_increment_columns(table)
        .map_err(|error| dml_storage_error(action, error))?
        .into_iter()
        .find(|(column, _)| column == auto_id_column)
        .is_some_and(|(_, provenance)| provenance.kind == uqa_sql::ast::AutoIncrementKind::Legacy);
    if !legacy {
        return Ok(());
    }
    let owner = uqa_sql::semantics::partition::partition_identity_owner(context.partitions, table)?;
    context
        .identifiers
        .persist_next_id(&owner)
        .map_err(|error| identifier_storage_error(action, &error))
}

pub fn prepare_insert_identity(
    context: InsertIdentityContext<'_>,
    allocation_table: &str,
    id_column: &str,
    accepts_supplied_identity: bool,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<(DocId, bool), SQLError> {
    let supplied_id = if accepts_supplied_identity {
        document_supplied_id(document, id_column, auto_id_column == Some(id_column))?
    } else {
        None
    };
    let supplied = supplied_id.is_some();
    let doc_id = match supplied_id {
        Some(doc_id) => doc_id,
        None => context.identifiers.allocate_next_id(allocation_table)?,
    };
    if auto_id_column == Some(id_column) {
        document.insert(id_column.to_string(), doc_id_value(doc_id)?);
    }
    context
        .identifiers
        .advance_next_id(allocation_table, doc_id)
        .map_err(|error| identifier_storage_error(action, &error))?;
    Ok((doc_id, supplied))
}

/// Allocate document identities using the partition hierarchy's identity owner.
pub struct IdentityAllocationContext<'a> {
    pub identifiers: &'a dyn MutationIdentifiers,
    pub partitions: &'a dyn PartitionCatalog,
}

pub fn refresh_insert_identity_after_trigger(
    allocation: IdentityAllocationContext<'_>,
    table: &str,
    id_column: &str,
    accepts_supplied_identity: bool,
    auto_id_column: Option<&str>,
    document: &Document,
    identity: &mut (DocId, bool),
) -> Result<(), SQLError> {
    if !accepts_supplied_identity {
        return Ok(());
    }
    let Some(doc_id) =
        document_supplied_id(document, id_column, auto_id_column == Some(id_column))?
    else {
        return Ok(());
    };
    if doc_id == identity.0 {
        return Ok(());
    }
    let owner =
        uqa_sql::semantics::partition::partition_identity_owner(allocation.partitions, table)?;
    allocation
        .identifiers
        .advance_next_id(&owner, doc_id)
        .map_err(|error| {
            identifier_storage_error("apply BEFORE INSERT trigger identity", &error)
        })?;
    *identity = (doc_id, true);
    Ok(())
}
