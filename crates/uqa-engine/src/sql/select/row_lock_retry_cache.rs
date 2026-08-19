//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Work-memory-bounded committed-row override store for tuple-local row-lock
//! rechecks. One instance exists per SQL statement; every locking scope of the
//! statement shares it so repeated occurrences of one changed tuple fetch the
//! committed override exactly once.

use parking_lot::Mutex;
use rusqlite::OptionalExtension;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use uqa_core::Value;
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::Engine;

pub(crate) struct RowLockRetryCache {
    state: Mutex<RetryCacheState>,
    row_locks: Arc<crate::row_locks::RowLockManager>,
    _change_observation: crate::row_locks::RowChangeObservation,
    snapshot_baseline: Mutex<crate::row_locks::RowChangeBaseline>,
}

struct RetryCacheState {
    budget_bytes: usize,
    memory_bytes: usize,
    memory: HashMap<Vec<u8>, Vec<u8>>,
    disk: Option<DiskRetryCache>,
}

struct DiskRetryCache {
    connection: rusqlite::Connection,
    _directory: TempDir,
}

/// Latest committed image of one row selected under a pending row lock. A
/// primary-key rewrite surfaces the successor identity so the recheck can lock
/// and return the row the blocker moved the tuple to.
#[derive(Clone)]
pub(crate) enum RetryRowOverride {
    Deleted,
    Present {
        doc_id: uqa_core::DocId,
        document: Document,
    },
}

impl RowLockRetryCache {
    pub(crate) fn new(
        budget_bytes: usize,
        row_locks: Arc<crate::row_locks::RowLockManager>,
        snapshot_baseline: crate::row_locks::RowChangeBaseline,
    ) -> Self {
        let change_observation = row_locks.begin_change_observation();
        Self {
            state: Mutex::new(RetryCacheState {
                budget_bytes: budget_bytes.max(1),
                memory_bytes: 0,
                memory: HashMap::new(),
                disk: None,
            }),
            row_locks,
            _change_observation: change_observation,
            snapshot_baseline: Mutex::new(snapshot_baseline),
        }
    }

    pub(crate) fn set_snapshot_baseline(&self, baseline: crate::row_locks::RowChangeBaseline) {
        *self.snapshot_baseline.lock() = baseline;
    }

    /// Report whether another transaction committed a change to `doc_id` that
    /// conflicts with the requested lock strength after this statement's
    /// snapshot, following primary-key rewrites to the final target identity.
    /// `PostgreSQL` 18 does not recheck a `FOR KEY SHARE` candidate after a
    /// compatible non-key update, so compatible mutation strengths return
    /// `None` here.
    pub(in crate::sql) fn conflicting_change_target_since_snapshot(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
        strength: uqa_sql::ast::LockStrength,
    ) -> Result<crate::row_locks::RowChangeTarget, SQLError> {
        let baseline = *self.snapshot_baseline.lock();
        self.row_locks
            .conflicting_change_target_after(table, doc_id, baseline, strength)
    }

    /// Fetch the latest committed image for a changed candidate row, memoized
    /// per (table, original doc id, lock strength) across duplicate occurrences
    /// in the statement. Different strengths cannot share an image because a
    /// non-key update may proceed after `FOR KEY SHARE` but conflict with a
    /// stronger later scope.
    pub(in crate::sql) fn committed_override(
        &self,
        engine: &Engine,
        table: &str,
        original_doc_id: uqa_core::DocId,
        target_doc_id: uqa_core::DocId,
        strength: uqa_sql::ast::LockStrength,
    ) -> Result<RetryRowOverride, SQLError> {
        let key = retry_row_key(table, original_doc_id, strength);
        if let Some(value) = self.lookup(&key)? {
            return decode_override(&value);
        }
        let document = engine.get_committed_document(table, target_doc_id)?;
        let row_override = match document {
            Some(document) => RetryRowOverride::Present {
                doc_id: target_doc_id,
                document,
            },
            None => RetryRowOverride::Deleted,
        };
        self.insert(key, &encode_override(&row_override)?)?;
        Ok(row_override)
    }

    fn lookup(&self, key: &[u8]) -> Result<Option<Value>, SQLError> {
        let state = self.state.lock();
        let encoded = if let Some(disk) = state.disk.as_ref() {
            disk.lookup(key)?
        } else {
            state.memory.get(key).cloned()
        };
        encoded.map(|encoded| decode_value(&encoded)).transpose()
    }

    fn insert(&self, key: Vec<u8>, value: &Value) -> Result<(), SQLError> {
        let encoded = encode_value(value)?;
        let mut state = self.state.lock();
        if let Some(disk) = state.disk.as_mut() {
            return disk.insert(&key, &encoded);
        }
        if state.memory.contains_key(&key) {
            return Ok(());
        }
        let entry_bytes = key
            .len()
            .checked_add(encoded.len())
            .ok_or_else(|| SQLError::Internal("row-lock retry cache entry size overflow".into()))?;
        let would_exceed = state
            .memory_bytes
            .checked_add(entry_bytes)
            .is_none_or(|bytes| bytes > state.budget_bytes);
        if would_exceed {
            state.migrate_to_disk()?;
            return state
                .disk
                .as_mut()
                .ok_or_else(|| SQLError::Internal("row-lock retry cache spill is absent".into()))?
                .insert(&key, &encoded);
        }
        state.memory_bytes += entry_bytes;
        state.memory.insert(key, encoded);
        Ok(())
    }
}

fn encode_override(row_override: &RetryRowOverride) -> Result<Value, SQLError> {
    Ok(match row_override {
        RetryRowOverride::Deleted => Value::List(vec![Value::Bool(false)]),
        RetryRowOverride::Present { doc_id, document } => {
            let doc_id = i64::try_from(*doc_id).map_err(|_| {
                SQLError::Internal(format!(
                    "row-lock retry override doc id {doc_id} exceeds the storable range"
                ))
            })?;
            Value::List(vec![
                Value::Bool(true),
                Value::Int(doc_id),
                Value::Map(document.clone()),
            ])
        }
    })
}

fn decode_override(value: &Value) -> Result<RetryRowOverride, SQLError> {
    match value {
        Value::List(values) if matches!(values.as_slice(), [Value::Bool(false)]) => {
            Ok(RetryRowOverride::Deleted)
        }
        Value::List(values) => match values.as_slice() {
            [Value::Bool(true), Value::Int(doc_id), Value::Map(document)] if *doc_id >= 0 => {
                Ok(RetryRowOverride::Present {
                    doc_id: *doc_id as uqa_core::DocId,
                    document: document.clone(),
                })
            }
            _ => Err(SQLError::Internal(
                "row-lock retry row has an invalid cache payload".into(),
            )),
        },
        _ => Err(SQLError::Internal(
            "row-lock retry row has an invalid cache payload".into(),
        )),
    }
}

fn retry_row_key(
    table: &str,
    doc_id: uqa_core::DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Vec<u8> {
    let mut key = Vec::with_capacity(32 + table.len());
    key.extend_from_slice(b"\0uqa-retry-row\0");
    key.extend_from_slice(&(table.len() as u64).to_be_bytes());
    key.extend_from_slice(table.as_bytes());
    key.extend_from_slice(&doc_id.to_be_bytes());
    key.push(match strength {
        uqa_sql::ast::LockStrength::ForKeyShare => 0,
        uqa_sql::ast::LockStrength::ForShare => 1,
        uqa_sql::ast::LockStrength::ForNoKeyUpdate => 2,
        uqa_sql::ast::LockStrength::ForUpdate => 3,
    });
    key
}

impl RetryCacheState {
    fn migrate_to_disk(&mut self) -> Result<(), SQLError> {
        if self.disk.is_some() {
            return Ok(());
        }
        let mut disk = DiskRetryCache::new()?;
        let entries = std::mem::take(&mut self.memory);
        disk.insert_all(entries)?;
        self.memory_bytes = 0;
        self.disk = Some(disk);
        Ok(())
    }
}

impl DiskRetryCache {
    fn new() -> Result<Self, SQLError> {
        let directory = tempfile::Builder::new()
            .prefix("uqa-row-lock-retry-")
            .tempdir()
            .map_err(|error| {
                SQLError::Internal(format!("create row-lock retry cache directory: {error}"))
            })?;
        let connection = rusqlite::Connection::open(directory.path().join("cache.sqlite"))
            .map_err(|error| SQLError::Internal(format!("open row-lock retry cache: {error}")))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; CREATE TABLE retry_cache (cache_key BLOB PRIMARY KEY, cache_value BLOB NOT NULL) WITHOUT ROWID",
            )
            .map_err(|error| {
                SQLError::Internal(format!("initialize row-lock retry cache: {error}"))
            })?;
        Ok(Self {
            connection,
            _directory: directory,
        })
    }

    fn lookup(&self, key: &[u8]) -> Result<Option<Vec<u8>>, SQLError> {
        self.connection
            .query_row(
                "SELECT cache_value FROM retry_cache WHERE cache_key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| SQLError::Internal(format!("read row-lock retry cache: {error}")))
    }

    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<(), SQLError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO retry_cache(cache_key, cache_value) VALUES (?1, ?2)",
                (key, value),
            )
            .map_err(|error| SQLError::Internal(format!("write row-lock retry cache: {error}")))?;
        Ok(())
    }

    fn insert_all(&mut self, entries: HashMap<Vec<u8>, Vec<u8>>) -> Result<(), SQLError> {
        let transaction = self.connection.transaction().map_err(|error| {
            SQLError::Internal(format!("begin row-lock retry cache spill: {error}"))
        })?;
        {
            let mut statement = transaction
                .prepare(
                    "INSERT OR IGNORE INTO retry_cache(cache_key, cache_value) VALUES (?1, ?2)",
                )
                .map_err(|error| {
                    SQLError::Internal(format!("prepare row-lock retry cache spill: {error}"))
                })?;
            for (key, value) in entries {
                statement.execute((&key, &value)).map_err(|error| {
                    SQLError::Internal(format!("spill row-lock retry cache entry: {error}"))
                })?;
            }
        }
        transaction.commit().map_err(|error| {
            SQLError::Internal(format!("commit row-lock retry cache spill: {error}"))
        })
    }
}

fn encode_value(value: &Value) -> Result<Vec<u8>, SQLError> {
    let mut encoded = Vec::new();
    ciborium::ser::into_writer(value, &mut encoded).map_err(|error| {
        SQLError::Internal(format!("encode row-lock retry override value: {error}"))
    })?;
    Ok(encoded)
}

fn decode_value(encoded: &[u8]) -> Result<Value, SQLError> {
    ciborium::de::from_reader(encoded).map_err(|error| {
        SQLError::Internal(format!("decode row-lock retry override value: {error}"))
    })
}
