//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned physical connection lease for an independently scoped transaction.

use std::ops::{Deref, DerefMut};

use rusqlite::Connection;

use crate::connection::PooledConnection;

/// An exclusive physical connection checked out from a managed pool. It is
/// independent of the originating logical session. Dropping the lease rolls
/// back an unfinished transaction before returning the connection to the pool;
/// a connection whose rollback fails is discarded.
pub struct SQLiteConnectionLease(pub(crate) PooledConnection);

impl Deref for SQLiteConnectionLease {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.0
            .connection()
            .expect("a live connection lease owns its checked-out connection")
    }
}

impl DerefMut for SQLiteConnectionLease {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
            .connection_mut()
            .expect("a live connection lease owns its checked-out connection")
    }
}

#[cfg(test)]
mod tests {
    use crate::{ManagedConnection, SQLiteError};

    #[test]
    fn auxiliary_pool_preserves_rollback_mode_and_rejects_wal_without_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auxiliary.db");
        let auxiliary = ManagedConnection::open_auxiliary(&path, None).unwrap();
        assert!(!auxiliary.supports_concurrent_pinned_read_and_write());
        assert!(auxiliary.data_version_monitor_is_nonblocking().unwrap());
        auxiliary.begin_deferred_transaction().unwrap();
        assert!(!auxiliary.data_version_monitor_is_nonblocking().unwrap());
        auxiliary.rollback_transaction().unwrap();
        drop(auxiliary);
        let wal = ManagedConnection::open(&path).unwrap();
        wal.with(|connection| {
            connection.execute_batch(
                "CREATE TABLE retained(value TEXT); INSERT INTO retained VALUES ('unchanged')",
            )?;
            Ok(())
        })
        .unwrap();
        drop(wal);
        let before = std::fs::read(&path).unwrap();
        let error = ManagedConnection::open_auxiliary(&path, None)
            .err()
            .expect("reject WAL");
        assert!(matches!(error, SQLiteError::AuxiliaryJournalMode(mode) if mode == "wal"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn encrypted_lease_reuses_its_connection_and_rolls_back_independently() {
        let directory = tempfile::tempdir().unwrap();
        let managed = ManagedConnection::open_encrypted(
            &directory.path().join("leases.db"),
            "lease-encryption-test-key",
        )
        .unwrap();
        {
            let leased = managed.lease_connection().unwrap();
            leased
                .execute_batch(
                    "CREATE TABLE retained(value TEXT); \
                 CREATE TEMP TABLE connection_identity(value INTEGER); \
                 INSERT INTO connection_identity VALUES (73); \
                 BEGIN IMMEDIATE; INSERT INTO retained VALUES ('must roll back')",
                )
                .unwrap();
        }
        let leased = managed.lease_connection().unwrap();
        assert!(leased.is_autocommit());
        let identity: i64 = leased
            .query_row("SELECT value FROM connection_identity", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            identity, 73,
            "the physical connection and key derivation are reused"
        );
        let retained: i64 = leased
            .query_row("SELECT count(*) FROM retained", [], |row| row.get(0))
            .unwrap();
        assert_eq!(retained, 0);

        // The held lease forces an independently keyed connection for the
        // logical session. Dropping the lease must not end that session.
        managed.begin_deferred_transaction().unwrap();
        leased
            .execute_batch("BEGIN IMMEDIATE; INSERT INTO retained VALUES ('separate')")
            .unwrap();
        drop(leased);
        managed
            .with(|connection| {
                assert!(!connection.is_autocommit());
                let retained: i64 =
                    connection.query_row("SELECT count(*) FROM retained", [], |row| row.get(0))?;
                assert_eq!(retained, 0);
                Ok(())
            })
            .unwrap();
        managed.rollback_transaction().unwrap();
    }
}
