//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog fixtures shared by the unit and integration harnesses.

use std::{path::Path, sync::Arc};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{Catalog, ManagedConnection, Result, SQLiteStorageBackend};

use super::Engine;

pub(crate) fn catalog(connection: ManagedConnection) -> Result<Catalog> {
    connection.bind_native_records(VersionedSessionOptions::default())?;
    Catalog::open(connection)
}

/// Construct the pre-conversion format so migration fixtures can edit historical physical rows before default Engine opening upgrades them.
pub(crate) fn legacy_engine(path: &Path) -> Engine {
    let connection = ManagedConnection::open(path).unwrap();
    let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
    let backend = Arc::new(SQLiteStorageBackend::new(connection));
    Engine::from_persistent_backends(catalog, backend).unwrap()
}
