//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A pending predecessor native mapping upgrades through the restore owner without admitting ordinary access.

use super::*;

const NATIVE_PREDECESSOR: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 12), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49), record_namespace BLOB NOT NULL CHECK(typeof(record_namespace) = 'blob' AND length(record_namespace) = 16))";

#[rstest::rstest]
fn interrupted_native_population_upgrade_preserves_restore_intent_and_original_history(
    #[values(0, 1, 2, 3)] mode: usize,
    #[values(false, true)] coordinator_published: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native-population-restore.db");
    let backup = seed(&path, mode, true);
    let control = control();
    {
        let connection = open(&path, mode).unwrap();
        let store = records(&connection, true);
        store.set_receipt_retention_limit(123, &control).unwrap();
        if coordinator_published {
            store
                .serializable_admission(&control)
                .unwrap()
                .publish_restored(backup.request, &control)
                .unwrap();
        }
        store
            .with(|sqlite| {
                let _permit = schema::WritePermit::acquire(sqlite)?;
                let transaction = schema::begin(sqlite)?;
                // Format 12 lacked later provider families.
                for table in [
                    "_foreign_server_metadata",
                    "_uqa_mvcc_native_sequence_values",
                    "_uqa_mvcc_native_vector_populations",
                    "_uqa_mvcc_native_vector_population_witnesses",
                ] {
                    assert_eq!(
                        transaction.query_row(
                            &format!("SELECT count(*) FROM {table}"),
                            [],
                            |row| row.get::<_, i64>(0)
                        )?,
                        0
                    );
                    transaction.execute_batch(&format!("DROP TABLE {table}"))?;
                }
                transaction.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
                transaction.execute_batch(NATIVE_PREDECESSOR)?;
                transaction.execute(
                    "INSERT INTO _uqa_mvcc_native_format VALUES(1,12,49,?1)",
                    [backup.namespace.unwrap().as_bytes().as_slice()],
                )?;
                for action in ["INSERT", "UPDATE", "DELETE"] {
                    transaction
                        .execute_batch(&schema::trigger("_uqa_mvcc_native_format", action).1)?;
                }
                transaction.execute(
                    "UPDATE _uqa_mvcc_metadata SET restore_target = ?1",
                    [backup.request.target().as_bytes().as_slice()],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .unwrap();
    }
    assert!(matches!(
        open(&path, mode),
        Err(SQLiteError::DatabaseRestoreIncomplete)
    ));
    let different = DatabaseRestore::new(backup.request.source()).unwrap();
    assert!(restored(&path, mode, different, &control).is_err());
    let _injection = inject(Boundary::IntentPublished, || {
        Err(SQLiteError::Io(std::io::Error::other("native upgrade reply lost")).into())
    });
    assert!(restored(&path, mode, backup.request, &control).is_err());
    assert!(matches!(
        open(&path, mode),
        Err(SQLiteError::DatabaseRestoreIncomplete)
    ));
    let connection = restored(&path, mode, backup.request, &control).unwrap();
    verify(&connection, &backup, true);
    connection.record_connection().with(|sqlite| {
        let (format, limit, pending): (i64, i64, Option<Vec<u8>>) = sqlite.query_row(
            "SELECT n.format, m.receipt_limit, m.restore_target FROM _uqa_mvcc_native_format n CROSS JOIN _uqa_mvcc_metadata m", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!((format, limit, pending), (16, 123, None));
        Ok(())
    }).unwrap();
    drop(connection);
    let retried = restored(&path, mode, backup.request, &control).unwrap();
    verify(&retried, &backup, true);
}
