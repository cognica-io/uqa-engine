//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, ManagedConnection, SQLiteRecordStore};

#[test]
fn native_vector_mapping_rejects_prior_writers_and_rollback_restores_the_old_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    SQLiteRecordStore::for_native(&connection, &control).unwrap();
    connection
        .with_physical(|sqlite| {
            let _permit = schema::WritePermit::acquire(sqlite).unwrap();
            let transaction = schema::begin(sqlite).unwrap();
            transaction
                .execute_batch("DROP TABLE _uqa_mvcc_native_format")
                .unwrap();
            transaction.execute_batch(FORMAT_SIX).unwrap();
            transaction
                .execute("INSERT INTO _uqa_mvcc_native_format VALUES (1,6,49)", [])
                .unwrap();
            for action in ["INSERT", "UPDATE", "DELETE"] {
                transaction
                    .execute_batch(&schema::trigger(TABLES[0].0, action).1)
                    .unwrap();
            }
            transaction.commit().unwrap();
            validate_format(sqlite, 6).unwrap();
            let transaction = schema::begin(sqlite).unwrap();
            reopen(&transaction, &control).unwrap();
            check_mapping_version(&transaction, 7).unwrap();
            assert!(check_mapping_version(&transaction, 6).is_err());
            // An error after migration must roll back its DDL and recreated guards together.
            drop(transaction);
            validate_format(sqlite, 6).unwrap();
            check_mapping_version(sqlite, 6).unwrap();
            assert!(check_mapping_version(sqlite, 7).is_err());
            Ok(())
        })
        .unwrap();
    SQLiteRecordStore::for_native(&connection, &control).unwrap();
    connection
        .with_physical(|sqlite| {
            validate_format(sqlite, 7).unwrap();
            assert!(check_mapping_version(sqlite, 6).is_err());
            Ok(())
        })
        .unwrap();
}
