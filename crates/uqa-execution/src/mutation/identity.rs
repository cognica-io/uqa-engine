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
mod source;
pub use reservation::reserve_document_id;
pub use source::IdentitySource;
use uqa_sql::semantics::key_identity::{is_key_document_id, key_document_id};

pub trait MutationIdentifiers {
    fn allocate_next_id(&self, table: &str) -> Result<DocId, SQLError>;
    /// Generate an identity for a row of `table` whose integer primary key names none: one at or above `KEY_IDENTITY_LIMIT`, where no key names an identity.
    fn allocate_unmapped_id(&self, table: &str) -> Result<DocId, SQLError>;
    /// Whether the single integer primary key of `table` names its rows' identities: each row whose key lies below `KEY_IDENTITY_LIMIT` has the identity equal to its key, and no other row has an identity below the limit. A table that does not keep this, as one an earlier version wrote can fail to, resolves its keys through the key's index.
    fn maps_integer_keys(&self, table: &str) -> Result<bool, SQLError>;
    fn advance_next_id(&self, table: &str, doc_id: DocId) -> uqa_storage::StorageBackendResult<()>;
    fn persist_next_id(&self, table: &str) -> uqa_storage::StorageBackendResult<()>;
    /// Whether every identity `allocate_next_id` returns for `table` is one no document of the table ever had. That holds where the table reserves identities in its own durable namespace, whose watermark every write of the table raises. A table that draws from another table's namespace, as a partition does, or keeps only a local counter cannot say.
    fn generates_unused_identities(&self, table: &str) -> Result<bool, SQLError>;
}
/// The single integer primary-key column of `table`, whose values name its rows' identities where the table maps them.
fn integer_primary_key_column(
    catalog: &dyn ConstraintCatalog,
    table: &str,
) -> Result<Option<String>, SQLError> {
    Ok(catalog
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("UPDATE primary key", err))?
        .and_then(|columns| {
            columns
                .into_iter()
                .find(|column| column.primary_key && column.ty.is_integer())
                .map(|column| column.name)
        }))
}

/// The identity a row holding `document` takes when it arrives in `table`, as an update moving it to another partition does: the one its integer primary key names, or one the table generates for a key that names none. `None` for a table whose key names no identities, which generates one as it does for any row.
pub fn arriving_key_identity(
    catalog: &dyn ConstraintCatalog,
    identifiers: &dyn MutationIdentifiers,
    table: &str,
    document: &Document,
) -> Result<Option<DocId>, SQLError> {
    let Some(key) = integer_primary_key_column(catalog, table)? else {
        return Ok(None);
    };
    if !identifiers.maps_integer_keys(table)? {
        return Ok(None);
    }
    Ok(Some(match document.get(&key).and_then(key_document_id) {
        Some(doc_id) => doc_id,
        None => identifiers.allocate_unmapped_id(table)?,
    }))
}

/// The identity an update moves a row of `table` to when `new_document` replaces it: the one its integer primary key names, or, for a key that names none, one the table generates when the row held an identity a key names. `None` keeps the row where it is. The caller decides once for each row and carries the decision to every later stage of the statement.
pub fn key_relocation(
    catalog: &dyn ConstraintCatalog,
    identifiers: &dyn MutationIdentifiers,
    table: &str,
    doc_id: DocId,
    new_document: &Document,
) -> Result<Option<DocId>, SQLError> {
    let Some(key) = integer_primary_key_column(catalog, table)? else {
        return Ok(None);
    };
    if !identifiers.maps_integer_keys(table)? {
        return Ok(None);
    }
    let relocated = match new_document.get(&key).and_then(key_document_id) {
        Some(named) => named,
        None if is_key_document_id(doc_id) => identifiers.allocate_unmapped_id(table)?,
        None => return Ok(None),
    };
    Ok((relocated != doc_id).then_some(relocated))
}

/// The identity an inserted row's identity column supplies, if any: an integer key the table maps, or the `id` of a table without declared columns. The caller generates one otherwise.
pub fn supplied_document_identity(
    source: IdentitySource,
    document: &Document,
    id_column: &str,
) -> Result<Option<DocId>, SQLError> {
    Ok(match source {
        IdentitySource::Generated => None,
        IdentitySource::IntegerKey => document.get(id_column).and_then(key_document_id),
        IdentitySource::Document => document_supplied_id(document, id_column),
    })
}

use crate::query::locking::context::QueryRowLockSession;
use uqa_sql::{
    assignment::{columns::AssignmentColumnCatalog, AssignmentContext},
    ast::{AutoIncrement, AutoIncrementKind, OverridingKind},
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
) -> Result<(Option<String>, String, IdentitySource), SQLError> {
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
        .collect::<Vec<_>>();
    let (id_column, source) = match primary_keys.as_slice() {
        [key] if key.ty.is_integer() => (key.name.clone(), IdentitySource::IntegerKey),
        [key] => (key.name.clone(), IdentitySource::Generated),
        _ if definitions.is_empty() => ("id".into(), IdentitySource::Document),
        _ => ("id".into(), IdentitySource::Generated),
    };
    // Only a key names a row. A SERIAL or identity column that is not the
    // table's single primary key is an ordinary column with a sequence
    // default, and two rows may supply the same value to it, so its value
    // cannot choose the physical document. The conventional `id` field is a
    // storage identity only for the legacy schema-less document API. A
    // declared SQL table without a primary key may contain duplicate ordinary
    // `id` values, so using that field as the physical document key would
    // silently replace rows.
    Ok((auto_increment, id_column, source))
}

/// Draw the values an inserted row leaves to its identity columns from their sequences, and select the identity of a row that a legacy counter column decides. A sequence value is an ordinary column value: when its column is the table's single primary key it names the row as any supplied key value does, so the caller selects the identity of every row this returns none for. A value the row supplies, NULL included, is kept; a `GENERATED ALWAYS` column accepts one only under `OVERRIDING SYSTEM VALUE`. `OVERRIDING USER VALUE` leaves every identity column out of the row before this runs.
#[expect(
    clippy::too_many_arguments,
    reason = "the identity columns and the OVERRIDING clause select one row's identity"
)]
pub fn prepare_auto_increment_identity(
    context: InsertIdentityContext<'_>,
    table: &str,
    id_column: &str,
    source: IdentitySource,
    auto_id_column: Option<&str>,
    overriding: Option<OverridingKind>,
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
        if document.contains_key(column) {
            if provenance.kind == AutoIncrementKind::IdentityAlways
                && overriding != Some(OverridingKind::SystemValue)
            {
                return Err(
                    uqa_sql::semantics::identity_columns::generated_always_insert_error(column),
                );
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
    if provenance.kind != AutoIncrementKind::Legacy {
        return Ok(None);
    }
    let owner = uqa_sql::semantics::partition::partition_identity_owner(context.partitions, table)?;
    context
        .locks
        .lock_relation(&owner, crate::row_locks::RelationLockMode::RowExclusive)?;
    if source == IdentitySource::IntegerKey && auto_id_column == id_column {
        return prepare_insert_identity(
            context,
            &owner,
            id_column,
            source,
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
        .filter(|_| !source.accepts_supplied())
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
        .is_some_and(|(_, provenance)| provenance.kind == AutoIncrementKind::Legacy);
    if !legacy {
        return Ok(());
    }
    let owner = uqa_sql::semantics::partition::partition_identity_owner(context.partitions, table)?;
    context
        .identifiers
        .persist_next_id(&owner)
        .map_err(|error| identifier_storage_error(action, &error))
}

/// Select the identity of a row `document` inserts into `allocation_table`, and whether the row supplied it. An integer key the table maps is the identity; a row whose integer key names none takes an identity the table generates at or above `KEY_IDENTITY_LIMIT`, except that a legacy counter key column the row leaves out draws its value from the table counter and that value names the row.
pub fn prepare_insert_identity(
    context: InsertIdentityContext<'_>,
    allocation_table: &str,
    id_column: &str,
    source: IdentitySource,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<(DocId, bool), SQLError> {
    let (doc_id, supplied) = match source {
        IdentitySource::Generated => (
            context.identifiers.allocate_next_id(allocation_table)?,
            false,
        ),
        IdentitySource::Document => match document_supplied_id(document, id_column) {
            Some(doc_id) => (doc_id, true),
            None => (
                context.identifiers.allocate_next_id(allocation_table)?,
                false,
            ),
        },
        IdentitySource::IntegerKey => match document
            .get(id_column)
            .filter(|value| !matches!(value, Value::Null))
        {
            Some(key) => match key_document_id(key) {
                Some(doc_id) if context.identifiers.maps_integer_keys(allocation_table)? => {
                    (doc_id, true)
                }
                _ => (
                    context.identifiers.allocate_unmapped_id(allocation_table)?,
                    false,
                ),
            },
            None if auto_id_column == Some(id_column) => {
                let doc_id = context.identifiers.allocate_next_id(allocation_table)?;
                document.insert(id_column.to_string(), doc_id_value(doc_id)?);
                (doc_id, false)
            }
            // A missing key fails its NOT NULL constraint; the row takes an identity no key names until then.
            None => (
                context.identifiers.allocate_unmapped_id(allocation_table)?,
                false,
            ),
        },
    };
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

/// Follow a `BEFORE INSERT` trigger that changed the row's identity column. A key the table maps moves the row to the identity it names; a key that names none takes an identity the table generates when the row held one a key names.
pub fn refresh_insert_identity_after_trigger(
    allocation: IdentityAllocationContext<'_>,
    table: &str,
    id_column: &str,
    source: IdentitySource,
    document: &Document,
    identity: &mut (DocId, bool),
) -> Result<(), SQLError> {
    let doc_id = match source {
        IdentitySource::Generated => return Ok(()),
        IdentitySource::Document => document_supplied_id(document, id_column),
        IdentitySource::IntegerKey => {
            let named = document.get(id_column).and_then(key_document_id);
            if named.is_some() && allocation.identifiers.maps_integer_keys(table)? {
                named
            } else {
                if is_key_document_id(identity.0) {
                    *identity = (allocation.identifiers.allocate_unmapped_id(table)?, false);
                }
                return Ok(());
            }
        }
    };
    let Some(doc_id) = doc_id else {
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
