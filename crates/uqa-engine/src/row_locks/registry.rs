//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared manager registry, relation identity allocation, and transaction IDs.

use super::{
    table_hash, Arc, AtomicU64, Condvar, CrossAttachment, FileLockCoordinator, HashMap,
    LockRelationIdentity, LockTable, ManagerIdentity, Mutex, OnceLock, Ordering, RowLockManager,
    RwLock, SQLError, Weak,
};

static DATABASE_MANAGERS: OnceLock<Mutex<HashMap<ManagerIdentity, Weak<RowLockManager>>>> =
    OnceLock::new();

impl RowLockManager {
    pub(crate) fn new() -> Self {
        Self::with_cross_attachment(None)
    }

    fn with_cross_attachment(cross: Option<CrossAttachment>) -> Self {
        Self {
            next_session: AtomicU64::new(1),
            next_transaction_xid: AtomicU64::new(3),
            relation_ids: Mutex::new(HashMap::new()),
            relation_identities: Mutex::new(HashMap::new()),
            next_table: AtomicU64::new(1),
            next_acquisition: AtomicU64::new(1),
            change_gate: RwLock::new(()),
            state: Mutex::new(LockTable {
                rows: HashMap::new(),
                waiting: HashMap::new(),
                relations: HashMap::new(),
                waiting_relations: HashMap::new(),
                advertised_waits: HashMap::new(),
                changes: Vec::new(),
                change_epoch: 0,
                active_change_observers: 0,
            }),
            wake: Condvar::new(),
            cross,
            column_stats: RwLock::new(std::collections::BTreeMap::new()),
        }
    }

    pub(super) fn for_database_file(path: &std::path::Path) -> Self {
        #[cfg(any(unix, windows))]
        {
            let cross = match FileLockCoordinator::open(path) {
                Ok(coordinator) => CrossAttachment::Active(Box::new(coordinator)),
                Err(reason) => CrossAttachment::Unavailable(reason),
            };
            Self::with_cross_attachment(Some(cross))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Self::with_cross_attachment(None)
        }
    }

    pub(crate) fn allocate_session(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn publish_column_stats(&self, table: String, stats: crate::ColumnStatsMap) {
        self.column_stats.write().insert(table, stats);
    }

    pub(crate) fn invalidate_column_stats(&self, table: &str) {
        self.column_stats.write().remove(table);
    }

    pub(crate) fn published_column_stats(&self, table: &str) -> Option<crate::ColumnStatsMap> {
        self.column_stats.read().get(table).cloned()
    }

    pub(crate) fn allocate_transaction_xid(&self) -> Result<u32, SQLError> {
        match self.cross.as_ref() {
            Some(CrossAttachment::Active(coordinator)) => {
                if let Some(xid) = coordinator
                    .allocate_transaction_xid()
                    .map_err(SQLError::Internal)?
                {
                    return Ok(xid);
                }
            }
            Some(CrossAttachment::Unavailable(reason)) => {
                return Err(SQLError::Internal(format!(
                    "cross-process transaction XID allocation is unavailable: {reason}"
                )));
            }
            None => {}
        }
        loop {
            let current = self.next_transaction_xid.load(Ordering::Relaxed);
            let xid = if (3..=u64::from(u32::MAX)).contains(&current) {
                u32::try_from(current).expect("validated transaction XID fits u32")
            } else {
                3
            };
            let following = if xid == u32::MAX {
                3
            } else {
                u64::from(xid + 1)
            };
            if self
                .next_transaction_xid
                .compare_exchange(current, following, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return Ok(xid);
            }
        }
    }

    pub(crate) fn table_key(&self, table: &str) -> u64 {
        self.relation_key(LockRelationIdentity::Table(Arc::from(table)))
    }

    pub(crate) fn backend_writer_key(&self) -> u64 {
        self.relation_key(LockRelationIdentity::BackendWriter)
    }

    pub(crate) fn key_reservation_key(&self, digest: [u8; 32]) -> u64 {
        self.relation_key(LockRelationIdentity::KeyReservation(digest))
    }

    fn relation_key(&self, identity: LockRelationIdentity) -> u64 {
        let mut relations = self.relation_ids.lock();
        if let Some(id) = relations.get(&identity) {
            return *id;
        }
        let id = self.next_table.fetch_add(1, Ordering::Relaxed);
        relations.insert(identity.clone(), id);
        self.relation_identities.lock().insert(id, identity);
        id
    }

    pub(crate) fn stable_table_hash(table: &str) -> u64 {
        table_hash(table.as_bytes())
    }

    pub(crate) fn table_name(&self, table: u64) -> Arc<str> {
        match self.relation_identities.lock().get(&table).cloned() {
            Some(LockRelationIdentity::Table(name)) => name,
            Some(_) => panic!("internal lock identity has no SQL table name"),
            None => panic!("unknown relation lock identity"),
        }
    }

    pub(super) fn relation_bytes(&self, table: u64) -> Vec<u8> {
        self.relation_identities
            .lock()
            .get(&table)
            .unwrap_or_else(|| panic!("unknown relation lock identity"))
            .stable_bytes()
    }

    pub(super) fn coordinator(&self) -> Result<Option<&FileLockCoordinator>, SQLError> {
        match self.cross.as_ref() {
            None => Ok(None),
            Some(CrossAttachment::Active(coordinator)) => Ok(Some(coordinator)),
            Some(CrossAttachment::Unavailable(reason)) => Err(SQLError::Internal(format!(
                "cross-process lock coordination is unavailable: {reason}"
            ))),
        }
    }

    /// Whether this manager has durable cross-process coordination. Unlike a peer-liveness check, this remains true after a peer exits, because commits made by that peer can still be newer than a statement snapshot.
    pub(crate) fn has_cross_process_coordination(&self) -> bool {
        matches!(self.cross.as_ref(), Some(CrossAttachment::Active(_)))
    }
}

pub(crate) fn shared_provider_manager(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    provider: &Arc<dyn uqa_storage::PersistentStorageProvider>,
) -> Arc<RowLockManager> {
    shared_manager(
        identity,
        ManagerIdentity::Provider(Arc::as_ptr(provider).cast::<()>() as usize),
    )
}

pub(crate) fn shared_backend_manager(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    backend: &Arc<dyn uqa_storage::PersistentStorageBackend>,
) -> Arc<RowLockManager> {
    shared_manager(
        identity,
        ManagerIdentity::Provider(Arc::as_ptr(backend).cast::<()>() as usize),
    )
}

fn shared_manager(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    fallback: ManagerIdentity,
) -> Arc<RowLockManager> {
    let identity = identity.map_or(fallback, ManagerIdentity::Durable);
    let registry = DATABASE_MANAGERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry.lock();
    registry.retain(|_, manager| manager.strong_count() != 0);
    if let Some(manager) = registry.get(&identity).and_then(Weak::upgrade) {
        return manager;
    }
    let manager = match &identity {
        ManagerIdentity::Durable(uqa_storage::PersistentStorageIdentity::File(path)) => {
            Arc::new(RowLockManager::for_database_file(path))
        }
        _ => Arc::new(RowLockManager::new()),
    };
    registry.insert(identity, Arc::downgrade(&manager));
    manager
}
