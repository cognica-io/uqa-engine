//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent storage backend factory.
//!
//! This boundary keeps the engine from constructing SQLite-backed stores
//! directly. Alternative persistent backends can implement the same factory
//! without changing query execution code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use uqa_analysis::Analyzer;
use uqa_core::{DocId, Value};

use crate::document_store::DocumentStore;
use crate::inverted_index::InvertedIndex;
use crate::vector_index::{VectorIndex, VectorIndexOpenMode, VectorIndexSpec};
use crate::CatalogFacade;

#[derive(Debug, thiserror::Error)]
pub enum StorageBackendError {
    #[error("text analysis failed: {0}")]
    Analysis(#[from] uqa_analysis::AnalysisError),
    #[error("payload serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("{backend} storage failed: {source}")]
    Backend {
        backend: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("{0}")]
    Other(String),
}

impl StorageBackendError {
    pub fn backend(
        backend: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Backend {
            backend,
            source: Box::new(source),
        }
    }
}

pub type StorageBackendResult<T> = std::result::Result<T, StorageBackendError>;

/// Opaque transaction checkpoint identity. SQL savepoint names remain engine metadata and are never forwarded into a backend namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageSavepointId(u64);

impl StorageSavepointId {
    #[must_use]
    pub fn allocate() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .expect("storage savepoint identity space exhausted");
        Self(id)
    }

    pub fn backend_name(self) -> String {
        self.0.to_string()
    }
}

/// Session-bound catalog and physical storage handles created together.
///
/// Both handles must share the same transaction context. Keeping their
/// construction behind one provider prevents a catalog write from escaping
/// through a different connection or transaction than document/index writes.
pub struct PersistentStorageSession {
    pub catalog: Arc<dyn CatalogFacade>,
    pub backend: Arc<dyn PersistentStorageBackend>,
}

/// Stable identity of one durable database. File identities allow engine coordination to extend across independently constructed providers and OS processes; opaque identities coordinate providers inside one process.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PersistentStorageIdentity {
    File(PathBuf),
    Opaque(String),
}

impl PersistentStorageIdentity {
    /// Resolve the stable file identity for a database path. The database file itself may not exist yet because backends materialize it on first write, so a missing file anchors the identity on its canonicalized parent directory instead of failing.
    pub fn for_database_path(path: &Path) -> StorageBackendResult<Self> {
        let path = resolve_final_symlinks(path)?;
        match std::fs::canonicalize(&path) {
            Ok(canonical) => Ok(Self::File(canonical)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let file_name = path.file_name().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "database path `{}` has no file name",
                        path.display()
                    ))
                })?;
                let parent = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                let parent = std::fs::canonicalize(&parent).map_err(|error| {
                    StorageBackendError::Other(format!(
                        "canonicalize database directory `{}`: {error}",
                        parent.display()
                    ))
                })?;
                Ok(Self::File(parent.join(file_name)))
            }
            Err(error) => Err(StorageBackendError::Other(format!(
                "canonicalize database `{}`: {error}",
                path.display()
            ))),
        }
    }
}

fn resolve_final_symlinks(path: &Path) -> StorageBackendResult<PathBuf> {
    let mut current = path.to_path_buf();
    let mut followed = 0usize;
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                followed += 1;
                if followed > 40 {
                    return Err(StorageBackendError::Other(format!(
                        "database path `{}` has too many symbolic-link levels",
                        path.display()
                    )));
                }
                let target = std::fs::read_link(&current).map_err(|error| {
                    StorageBackendError::Other(format!(
                        "read database symbolic link `{}`: {error}",
                        current.display()
                    ))
                })?;
                current = if target.is_absolute() {
                    target
                } else {
                    current
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new("."))
                        .join(target)
                };
            }
            Ok(_) => return Ok(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(current),
            Err(error) => {
                return Err(StorageBackendError::Other(format!(
                    "inspect database path `{}`: {error}",
                    current.display()
                )))
            }
        }
    }
}

impl PersistentStorageSession {
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> Self {
        Self { catalog, backend }
    }
}

/// Factory for independent sessions over one durable database.
///
/// A provider owns the database-level resource while each returned session
/// owns its transaction state. This is the engine-facing extension point for
/// `SQLite`, redb, and application-defined Key/Value stores.
pub trait PersistentStorageProvider: Send + Sync {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession>;

    /// Open handles for initial Engine restoration. Providers may defer catalog schema preparation until `CatalogFacade::initialize_storage` runs inside the owning transaction; ordinary session factories must return an initialized catalog.
    fn open_initial_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        self.open_session()
    }

    /// Return the database identity shared by every session this provider opens. Custom providers that cannot expose a stable identity may keep the default; engines built from the same `Arc` provider still share an in-process coordinator.
    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        Ok(None)
    }
}

/// Factory plus transaction surface for persistent table/index storage.
pub trait PersistentStorageBackend: Send + Sync {
    /// Return the stable database identity for independently constructed engines over this backend. File identities also enable cross-process row-lock coordination.
    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        Ok(None)
    }

    /// Open a transaction-isolated catalog/backend pair over the same durable database. Engines constructed from already-open backends retain this factory so a row-lock recheck can read the latest committed tuple while the caller's statement snapshot remains pinned.
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        Err(StorageBackendError::Other(
            "independent sessions are not implemented for this persistent backend".into(),
        ))
    }

    /// Whether an independently pinned read session can remain open while another session writes the same database. Backends that return `false` use a detached fixed snapshot for `REPEATABLE READ` and `SERIALIZABLE` transactions.
    fn supports_concurrent_pinned_read_and_write(&self) -> bool {
        false
    }

    fn document_store(&self, table: &str) -> Box<dyn DocumentStore>;

    /// Upgrade backend-owned document records before table handles are restored. Implementations must make the rewrite atomic and idempotent.
    fn migrate_document_storage(&self) -> StorageBackendResult<()> {
        Ok(())
    }

    fn inverted_index(&self, table: &str, analyzer: Analyzer) -> Box<dyn InvertedIndex>;

    /// Upgrade backend-owned inverted-index values before table handles are
    /// restored. Implementations must make the rewrite atomic and idempotent.
    fn migrate_inverted_index_storage(&self) -> StorageBackendResult<()> {
        Ok(())
    }

    fn vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
        mode: VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn VectorIndex>>;

    fn drop_vector_index_metadata(&self, _table: &str, _field: &str) -> StorageBackendResult<()> {
        Ok(())
    }

    /// Whether this backend maps logical `btree` indexes to durable postings.
    fn persists_btree_indexes(&self) -> bool {
        false
    }

    /// `Some(entries)` is a complete persisted index; `None` means it has not
    /// been built yet and the engine must backfill it from documents once.
    fn load_btree_index(
        &self,
        _table: &str,
        _field: &crate::ValueIndexKey,
    ) -> StorageBackendResult<Option<Vec<(DocId, Value)>>> {
        Ok(None)
    }

    fn btree_index_fields(&self, _table: &str) -> StorageBackendResult<Vec<crate::ValueIndexKey>> {
        Ok(Vec::new())
    }

    /// Fields whose persisted posting support was found inconsistent during a
    /// schema migration. The engine repairs these at its explicit open-time
    /// write boundary and clears each durable retry marker only after success.
    fn btree_index_repairs(&self) -> StorageBackendResult<Vec<(String, crate::ValueIndexKey)>> {
        Ok(Vec::new())
    }

    fn clear_btree_index_repair(
        &self,
        _table: &str,
        _field: &crate::ValueIndexKey,
    ) -> StorageBackendResult<()> {
        Ok(())
    }

    fn replace_btree_index(
        &self,
        _table: &str,
        _field: &crate::ValueIndexKey,
        _values: &[(DocId, Value)],
    ) -> StorageBackendResult<()> {
        Ok(())
    }

    /// Repair sparse support differences without requiring capable backends
    /// to rewrite every already-valid posting. The complete replacement is
    /// supplied for the storage-neutral fallback.
    fn repair_btree_index(
        &self,
        table: &str,
        field: &crate::ValueIndexKey,
        complete: &[(DocId, Value)],
        _stale_doc_ids: &[DocId],
        _missing: &[(DocId, Value)],
    ) -> StorageBackendResult<()> {
        self.replace_btree_index(table, field, complete)
    }

    /// Replace several complete indexes for one table atomically. Backends
    /// may override this to share one transaction and prepared statements.
    fn replace_btree_indexes(
        &self,
        table: &str,
        indexes: &[(&crate::ValueIndexKey, &[(DocId, Value)])],
    ) -> StorageBackendResult<()> {
        for (field, values) in indexes {
            self.replace_btree_index(table, field, values)?;
        }
        Ok(())
    }

    fn apply_btree_index_write(
        &self,
        _table: &str,
        _doc_id: DocId,
        _values: Option<&BTreeMap<crate::ValueIndexKey, Value>>,
    ) -> StorageBackendResult<()> {
        Ok(())
    }

    fn drop_btree_index(
        &self,
        _table: &str,
        _field: &crate::ValueIndexKey,
    ) -> StorageBackendResult<()> {
        Ok(())
    }

    fn clear_btree_indexes(&self, _table: &str) -> StorageBackendResult<()> {
        Ok(())
    }

    /// Reclaim backend-owned storage outside a transaction. Backends whose logical stores eagerly remove obsolete values may keep the no-op default; durable backends with file-level compaction should override it.
    fn vacuum(&self) -> StorageBackendResult<()> {
        Ok(())
    }

    fn begin_transaction(&self) -> StorageBackendResult<()>;

    /// Begin a transaction whose first operation is expected to be a read.
    /// Backends with distinct lock modes may defer write-lock acquisition;
    /// the default preserves existing transaction semantics.
    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.begin_transaction()
    }

    /// Begin an atomic transaction that may remain read-only or perform writes after its initial reads. Backends without read-to-write promotion acquire a writer transaction immediately.
    fn begin_upgradeable_transaction(&self) -> StorageBackendResult<()> {
        self.begin_transaction()
    }

    /// Whether this session currently owns a pinned storage transaction.
    fn in_transaction(&self) -> bool;

    /// Whether the current transaction has performed a physical write.
    ///
    /// The engine uses this to enforce read-only statement transactions even
    /// for writes made through catalog/index helpers it did not classify.
    fn transaction_has_written(&self) -> StorageBackendResult<bool>;

    /// Backend commit generation visible to this session, when available.
    /// A changing value invalidates session-local catalog and index caches.
    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        Ok(None)
    }

    /// Whether reading [`Self::change_version`] can proceed while this
    /// session owns its write transaction.
    fn change_version_monitor_is_nonblocking(&self) -> StorageBackendResult<bool> {
        Ok(true)
    }

    /// Pin the transaction's read snapshot before cache restoration.
    fn pin_transaction_snapshot(&self) -> StorageBackendResult<()> {
        Ok(())
    }

    fn commit_transaction(&self) -> StorageBackendResult<()>;

    fn rollback_transaction(&self) -> StorageBackendResult<()>;

    fn savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()>;

    fn release_savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()>;

    fn rollback_to_savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()>;
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn dangling_database_symlink_keeps_the_target_identity_after_creation() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.db");
        let link = directory.path().join("database.db");
        symlink("target.db", &link).unwrap();

        let before = PersistentStorageIdentity::for_database_path(&link).unwrap();
        std::fs::File::create(&target).unwrap();
        let after = PersistentStorageIdentity::for_database_path(&link).unwrap();

        assert_eq!(before, after);
        assert_eq!(
            after,
            PersistentStorageIdentity::File(target.canonicalize().unwrap())
        );
    }
}
