//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical connection pooling retains database ownership through final close.

use parking_lot::{Condvar, Mutex};
use rusqlite::Connection;
use std::sync::Arc;

use super::{
    ownership::DatabaseOwner, snapshot::PhysicalConnection, ConnectionSpec, ManagedConnection,
    Result, SQLiteError,
};

struct PoolState {
    idle: Vec<PhysicalConnection>,
    open: usize,
}

pub(super) struct ConnectionPool {
    pub(super) memory_identity: Mutex<Option<String>>,
    pub(super) serializable_leases: Mutex<Option<Arc<uqa_storage::mvcc::LocalSerializableLeases>>>,
    pub(super) receipt_state: Arc<uqa_storage::mvcc::LocalSerializableState>,
    pub(super) serializable_connection:
        Mutex<Option<(uqa_storage::mvcc::DatabaseId, ManagedConnection)>>,
    pub(super) snapshot_registry: Mutex<
        Option<(
            uqa_storage::mvcc::DatabaseId,
            std::sync::Weak<uqa_storage::mvcc::SnapshotRegistry>,
        )>,
    >,
    pub(super) spec: ConnectionSpec,
    max_connections: usize,
    state: Mutex<PoolState>,
    available: Condvar,
    /// Stable, never-mutating connection used for `PRAGMA data_version`.
    /// Every logical session over this pool must compare versions on this
    /// same connection, and encrypted databases must not repeat key
    /// derivation merely to create a request-local change monitor.
    pub(super) data_version_monitor: Mutex<Option<Connection>>,
    // Keep restoration excluded until every physical connection has closed.
    pub(super) owner: Option<Arc<DatabaseOwner>>,
}

impl ConnectionPool {
    pub(super) fn new(
        spec: ConnectionSpec,
        initial: Connection,
        max_connections: usize,
        owner: Option<Arc<DatabaseOwner>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            memory_identity: Mutex::new(None),
            serializable_leases: Mutex::new(None),
            receipt_state: Arc::default(),
            serializable_connection: Mutex::new(None),
            snapshot_registry: Mutex::new(None),
            spec,
            max_connections: max_connections.max(1),
            state: Mutex::new(PoolState {
                idle: vec![PhysicalConnection::new(initial)],
                open: 1,
            }),
            available: Condvar::new(),
            data_version_monitor: Mutex::new(None),
            owner,
        })
    }

    pub(super) fn checkout(self: &Arc<Self>) -> Result<PooledConnection> {
        self.checkout_with_cancellation(None)
    }

    pub(super) fn checkout_with_cancellation(
        self: &Arc<Self>,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> Result<PooledConnection> {
        loop {
            if let Some(cancellation) = cancellation {
                cancellation.check()?;
            }
            let mut state = self.state.lock();
            if let Some(connection) = state.idle.pop() {
                return Ok(PooledConnection {
                    pool: Arc::clone(self),
                    connection: Some(connection),
                });
            }
            if state.open < self.max_connections {
                state.open += 1;
                drop(state);
                return match self.spec.open(false) {
                    Ok(connection) => Ok(PooledConnection {
                        pool: Arc::clone(self),
                        connection: Some(PhysicalConnection::new(connection)),
                    }),
                    Err(error) => {
                        let mut state = self.state.lock();
                        state.open -= 1;
                        self.available.notify_one();
                        Err(error)
                    }
                };
            }
            if cancellation.is_some() {
                self.available
                    .wait_for(&mut state, std::time::Duration::from_millis(10));
            } else {
                self.available.wait(&mut state);
            }
        }
    }

    fn checkin(&self, connection: PhysicalConnection) {
        self.state.lock().idle.push(connection);
        self.available.notify_one();
    }

    fn discard(&self) {
        let mut state = self.state.lock();
        state.open -= 1;
        self.available.notify_one();
    }
}

pub(crate) struct PooledConnection {
    pool: Arc<ConnectionPool>,
    connection: Option<PhysicalConnection>,
}

impl PooledConnection {
    pub(crate) fn connection(&self) -> Result<&Connection> {
        self.connection
            .as_ref()
            .map(|physical| &physical.connection)
            .ok_or(SQLiteError::MissingCheckedOutConnection)
    }

    pub(super) fn physical_mut(&mut self) -> Result<&mut PhysicalConnection> {
        self.connection
            .as_mut()
            .ok_or(SQLiteError::MissingCheckedOutConnection)
    }

    pub(crate) fn connection_mut(&mut self) -> Result<&mut Connection> {
        self.physical_mut().map(|physical| &mut physical.connection)
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        let reusable = connection.connection.is_autocommit()
            || connection.connection.execute_batch("ROLLBACK").is_ok();
        if reusable {
            self.pool.checkin(connection);
        } else {
            self.pool.discard();
        }
    }
}
