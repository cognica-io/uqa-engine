//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native row operations share one logical session, including the read boundary of each evaluated mutation.

use uqa_storage::mvcc::{VersionError, VersionedPersistence, VersionedSessionOptions};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{KeyValueBatch, PersistentStorageIdentity};

use super::logical::BoundRecordSession;
use super::{Arc, KeyValueStore, ManagedConnection, Result, SQLiteError, VersionedKeyValueStore};
use crate::mvcc::native::NativeSnapshot;

impl ManagedConnection {
    pub(crate) fn allocate_native_identifiers(
        &self,
        namespace: &[u8],
        request: uqa_storage::mvcc::IdentifierRequest,
    ) -> Result<uqa_storage::mvcc::IdentifierAllocation> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        let logical = self
            .session
            .logical
            .get()
            .filter(|session| session.native.is_some())
            .ok_or(SQLiteError::SessionMappingMismatch)?;
        logical
            .allocate_identifiers(namespace, request)
            .map_err(Into::into)
    }

    pub(crate) fn is_native_record_session(&self) -> bool {
        self.session
            .logical
            .get()
            .is_some_and(|session| session.native.is_some())
    }

    /// Bind an initialized native catalog or recognized standalone graph file to shared logical record transactions, converting its physical format if needed. Document/B-tree stores, exact/IVF/HNSW vectors, occurrences and catalog operations use this session, including namespace validation during explicit provider restoration. Bind before opening a catalog on an already converted file. Standalone graph handles use scoped records on the same session, including handles opened before binding. Direct physical access is rejected. This development entry point does not enable concurrent Engine SQL.
    pub fn bind_native_records(&self, options: VersionedSessionOptions) -> Result<()> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        if let Some(logical) = self.session.logical.get() {
            if logical.native.is_none() {
                return Err(SQLiteError::SessionMappingMismatch);
            }
            return if logical.options().retained_bytes == options.retained_bytes {
                Ok(())
            } else {
                Err(SQLiteError::SessionOptionsMismatch)
            };
        }
        if self.session.transaction.lock().is_some() {
            return Err(SQLiteError::TransactionAlreadyActive);
        }
        let identity = self
            .database_path()
            .map(PersistentStorageIdentity::for_database_path)
            .transpose()?;
        let records = crate::SQLiteRecordStore::for_native(
            &self.record_connection(),
            &StorageReadControl::with_limit(options.retained_bytes),
        )
        .map_err(VersionError::into_storage_error)?;
        let database = records.database_id();
        self.session
            .logical
            .set(Arc::new(BoundRecordSession {
                store: Arc::new(VersionedKeyValueStore::new(
                    Arc::new(records),
                    identity,
                    options,
                )),
                native: Some(database),
            }))
            .map_err(|_| SQLiteError::SessionMappingMismatch)
    }

    pub(crate) fn native_snapshot(&self) -> Result<Option<Arc<NativeSnapshot>>> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        self.session
            .logical
            .get()
            .map(|logical| {
                NativeSnapshot::capture(
                    logical,
                    logical.native.ok_or(SQLiteError::SessionMappingMismatch)?,
                )
                .map(Arc::new)
            })
            .transpose()
    }

    pub(crate) fn with_native_write<R>(
        &self,
        operation: impl FnOnce(&NativeSnapshot, &mut dyn KeyValueBatch) -> Result<R>,
    ) -> Result<Option<R>> {
        self.surface_cleanup_failure()?;
        // This gate covers read/evaluate/stage, not just each individual byte-store call.
        let _gate = self.session.gate.write();
        let Some(logical) = self.session.logical.get() else {
            return Ok(None);
        };
        let database = logical.native.ok_or(SQLiteError::SessionMappingMismatch)?;
        let own_transaction = !logical.in_transaction();
        if own_transaction {
            logical.begin_transaction()?;
        }
        let mut scope = WriteScope {
            connection: self,
            logical,
            rollback: own_transaction,
        };
        let snapshot = NativeSnapshot::capture(logical, database)?;
        let mut batch = logical.batch();
        let result = operation(&snapshot, &mut *batch)?;
        batch.commit()?;
        // A failed publication keeps its evaluated attempt for explicit receipt resolution.
        scope.rollback = false;
        if own_transaction {
            logical.commit_transaction()?;
        }
        Ok(Some(result))
    }
}

struct WriteScope<'a> {
    connection: &'a ManagedConnection,
    logical: &'a BoundRecordSession,
    rollback: bool,
}

impl Drop for WriteScope<'_> {
    fn drop(&mut self) {
        if self.rollback {
            if let Err(error) = self.logical.rollback_transaction() {
                *self.connection.session.cleanup_failure.lock() = Some(error.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_evaluation_unwind_rolls_back_only_its_own_transaction() {
        let connection = ManagedConnection::open_in_memory().unwrap();
        crate::Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        for explicit in [false, true] {
            if explicit {
                connection.begin_transaction().unwrap();
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _: Result<Option<()>> = connection.with_native_write(|snapshot, batch| {
                    snapshot.ensure_table_owner("never-published", batch)?;
                    panic!("injected evaluation unwind");
                });
            }));
            assert!(result.is_err());
            assert_eq!(connection.in_transaction(), explicit);
            assert!(connection
                .native_snapshot()
                .unwrap()
                .unwrap()
                .table_owner("never-published")
                .unwrap()
                .is_none());
            if explicit {
                connection.rollback_transaction().unwrap();
            }
        }
    }

    #[test]
    fn native_document_writes_respect_a_read_only_logical_transaction() {
        use uqa_storage::DocumentStore;
        let connection = ManagedConnection::open_in_memory().unwrap();
        crate::Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        connection.begin_record_read().unwrap();
        let mut documents = crate::SQLiteDocumentStore::new(connection.clone(), "docs");
        assert!(documents.put(1, std::collections::BTreeMap::new()).is_err());
        assert!(!connection.transaction_has_written().unwrap());
        connection.commit_transaction().unwrap();
        assert!(!documents.contains_doc_id(1).unwrap());
    }

    #[test]
    fn native_autocommit_pins_preconditions_before_evaluating_mutation() {
        use crate::mvcc::native::{NativeRecord, NativeRecordFamily};
        use uqa_storage::DocumentStore;
        let connection = ManagedConnection::open_in_memory().unwrap();
        crate::Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let mut a = crate::SQLiteDocumentStore::new(connection.clone(), "docs");
        let fields = |n| std::collections::BTreeMap::from([("n".into(), uqa_core::Value::Int(n))]);
        a.put(1, fields(1)).unwrap();
        let mut b = crate::SQLiteDocumentStore::new(connection.new_session(), "docs");
        let result = connection.with_native_write(|snapshot, batch| {
            let owner = snapshot.table_owner("docs")?.unwrap();
            let old = snapshot
                .read_row(
                    NativeRecordFamily::Documents,
                    owner,
                    &[rusqlite::types::ValueRef::Integer(1)],
                    |row| {
                        Ok(NativeRecord::encode(
                            NativeRecordFamily::Documents,
                            owner,
                            row,
                            &snapshot.control,
                        )?)
                    },
                )?
                .unwrap();
            // B publishes after A's evaluation read but before A stages its result.
            b.put(1, fields(2))?;
            batch.put(old.key(), old.row())?;
            Ok(())
        });
        assert!(result.is_err());
        connection.rollback_transaction().unwrap();
        assert_eq!(a.get(1).unwrap(), Some(fields(2)));
    }
}
