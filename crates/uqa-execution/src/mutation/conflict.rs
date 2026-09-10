//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded conflict arbitration for rows produced by a single INSERT command.
use super::{
    candidate::PhysicalDocumentIdentity,
    constraints::{index_keys::EnforcedKeyExecution, ConstraintContext},
    errors::dml_storage_error,
};
use rusqlite::OptionalExtension;
use uqa_sql::{plan::ConflictPlan, SQLError};
use uqa_storage::document_store::Document;

pub enum CurrentInsertConflict {
    Overlay,
    Base(PhysicalDocumentIdentity),
}

/// Disk-backed view of rows inserted or rewritten earlier by the same INSERT command. `PostgreSQL` resolves `ON CONFLICT` inputs sequentially: a key moved away by an earlier update becomes insertable, while a later `DO UPDATE` that reaches a row already inserted or updated by the command raises 21000. Keeping the exact key index in a temporary `SQLite` database preserves those semantics without retaining a cardinality-sized map above `work_mem`.
pub struct InsertConflictOverlay {
    connection: rusqlite::Connection,
    _directory: tempfile::TempDir,
    constraints: Vec<uqa_sql::catalog::index::EnforcedKey>,
    relevant_constraints: Vec<usize>,
    next_insert_identity: u64,
}

impl InsertConflictOverlay {
    pub fn new(
        context: ConstraintContext<'_>,
        table: &str,
        on_conflict: &ConflictPlan,
    ) -> Result<Self, SQLError> {
        let constraints = context
            .catalog
            .enforced_keys(table)
            .map_err(|error| dml_storage_error("INSERT conflict overlay", error))?;
        let relevant_constraints = uqa_sql::semantics::conflict::conflict_key_indices(
            context.catalog,
            table,
            &constraints,
            on_conflict,
        )?;
        let directory = tempfile::Builder::new()
            .prefix("uqa-insert-conflict-")
            .tempdir()
            .map_err(|error| {
                SQLError::Internal(format!("create INSERT conflict overlay directory: {error}"))
            })?;
        let connection = rusqlite::Connection::open(directory.path().join("overlay.sqlite"))
            .map_err(|error| {
                SQLError::Internal(format!("open INSERT conflict overlay: {error}"))
            })?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = OFF;
                 PRAGMA synchronous = OFF;
                 CREATE TABLE overlay_keys (
                     physical_table TEXT NOT NULL,
                     constraint_index INTEGER NOT NULL,
                     key BLOB NOT NULL,
                     identity BLOB NOT NULL,
                     PRIMARY KEY (physical_table, constraint_index, key)
                 ) WITHOUT ROWID;
                 CREATE TABLE overridden_documents (
                     physical_table TEXT NOT NULL,
                     doc_id BLOB NOT NULL,
                     PRIMARY KEY (physical_table, doc_id)
                 ) WITHOUT ROWID;",
            )
            .map_err(|error| {
                SQLError::Internal(format!("initialize INSERT conflict overlay: {error}"))
            })?;
        Ok(Self {
            connection,
            _directory: directory,
            constraints,
            relevant_constraints,
            next_insert_identity: 0,
        })
    }

    pub fn find(
        &self,
        context: ConstraintContext<'_>,
        table: &str,
        document: &Document,
    ) -> Result<Option<CurrentInsertConflict>, SQLError> {
        for &index in &self.relevant_constraints {
            let constraint = &self.constraints[index];
            let constraint_index = i64::try_from(index).map_err(|_| {
                SQLError::Internal("INSERT conflict constraint index exceeds i64".into())
            })?;
            let Some(values) = constraint.values(context, table, document)? else {
                continue;
            };
            let key =
                crate::canonical_row_key(&values).map_err(crate::physical::physical_exec_error)?;
            let overlay = self
                .connection
                .query_row(
                    "SELECT 1 FROM overlay_keys WHERE physical_table = ?1 AND constraint_index = ?2 AND key = ?3",
                    rusqlite::params![table, constraint_index, key],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|error| {
                    SQLError::Internal(format!("probe INSERT conflict overlay: {error}"))
                })?;
            if overlay.is_some() {
                return Ok(Some(CurrentInsertConflict::Overlay));
            }
            let Some(doc_id) = constraint.find_conflict(context, table, &values, None)? else {
                continue;
            };
            let overridden = self
                .connection
                .query_row(
                    "SELECT 1 FROM overridden_documents WHERE physical_table = ?1 AND doc_id = ?2",
                    rusqlite::params![table, doc_id.to_be_bytes().as_slice()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "probe overridden INSERT conflict document: {error}"
                    ))
                })?;
            if overridden.is_none() {
                return Ok(Some(CurrentInsertConflict::Base(
                    PhysicalDocumentIdentity {
                        table: table.to_string(),
                        doc_id,
                    },
                )));
            }
        }
        Ok(None)
    }

    pub fn note_insert(
        &mut self,
        context: ConstraintContext<'_>,
        table: &str,
        document: &Document,
    ) -> Result<(), SQLError> {
        let mut identity = Vec::with_capacity(9);
        identity.push(b'i');
        identity.extend_from_slice(&self.next_insert_identity.to_be_bytes());
        self.next_insert_identity = self.next_insert_identity.checked_add(1).ok_or_else(|| {
            SQLError::Internal("INSERT conflict overlay identity space is exhausted".into())
        })?;
        self.note_keys(context, table, &identity, document)
    }

    pub fn note_update(
        &mut self,
        context: ConstraintContext<'_>,
        base: &PhysicalDocumentIdentity,
        document: &Document,
    ) -> Result<(), SQLError> {
        let mut overlay_identity = Vec::with_capacity(9);
        overlay_identity.push(b'b');
        overlay_identity.extend_from_slice(&base.doc_id.to_be_bytes());
        self.connection
            .execute(
                "INSERT OR IGNORE INTO overridden_documents (physical_table, doc_id) VALUES (?1, ?2)",
                rusqlite::params![base.table, base.doc_id.to_be_bytes().as_slice()],
            )
            .map_err(|error| {
                SQLError::Internal(format!(
                    "record overridden INSERT conflict document: {error}"
                ))
            })?;
        self.note_keys(context, &base.table, &overlay_identity, document)
    }

    fn note_keys(
        &mut self,
        context: ConstraintContext<'_>,
        table: &str,
        identity: &[u8],
        document: &Document,
    ) -> Result<(), SQLError> {
        for (index, constraint) in self.constraints.iter().enumerate() {
            let constraint_index = i64::try_from(index).map_err(|_| {
                SQLError::Internal("INSERT conflict constraint index exceeds i64".into())
            })?;
            let Some(values) = constraint.values(context, table, document)? else {
                continue;
            };
            let key =
                crate::canonical_row_key(&values).map_err(crate::physical::physical_exec_error)?;
            self.connection
                .execute(
                    "INSERT OR IGNORE INTO overlay_keys (physical_table, constraint_index, key, identity) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![table, constraint_index, key, identity],
                )
                .map_err(|error| {
                    SQLError::Internal(format!("record INSERT conflict overlay key: {error}"))
                })?;
        }
        Ok(())
    }
}

pub fn find_insert_conflict(
    context: ConstraintContext<'_>,
    table: &str,
    on_conflict: &ConflictPlan,
    document: &Document,
) -> Result<Option<PhysicalDocumentIdentity>, SQLError> {
    let constraints = context
        .catalog
        .enforced_keys(table)
        .map_err(|err| dml_storage_error("INSERT conflict lookup", err))?;
    for index in uqa_sql::semantics::conflict::conflict_key_indices(
        context.catalog,
        table,
        &constraints,
        on_conflict,
    )? {
        let constraint = &constraints[index];
        let Some(values) = constraint.values(context, table, document)? else {
            continue;
        };
        if let Some(doc_id) = constraint.find_conflict(context, table, &values, None)? {
            return Ok(Some(PhysicalDocumentIdentity {
                table: table.to_string(),
                doc_id,
            }));
        }
    }
    Ok(None)
}

pub mod update;
