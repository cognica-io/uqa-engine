//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh-target drills validate Engine observations while the repository target is still private.

use super::*;
use uqa_storage::{
    backup::{BackupLease, BackupRestoreImporter, RestoreDirectory},
    mvcc::DatabaseRestore,
    StorageEncryptionKey, StorageTransactionModel,
};
use uqa_storage_redb::{RedbBackupImport, RedbBackupImportStep};
use uqa_storage_sqlite::{
    SQLiteBackupImportOptions, SQLiteBackupImportStep, SQLiteKeyValueBackupImport,
    SQLiteNativeBackupImport,
};

struct Importer<'a> {
    kind: Provider,
    mode: usize,
    expected: &'a Observed,
}

impl Importer<'_> {
    fn options(&self) -> SQLiteBackupImportOptions {
        SQLiteBackupImportOptions {
            compression: (self.mode >= 2).then(SQLiteCompressionOptions::default),
            encryption_key: (self.mode % 2 == 1).then(|| StorageEncryptionKey::new(KEY)),
            maximum_value_bytes: 4 << 20,
            maximum_workspace_bytes: 16 << 20,
        }
    }

    fn validate(&self, path: &Path, request: DatabaseRestore) {
        let provider = self.kind.open(path, self.mode);
        let session = provider.open_session().unwrap();
        assert_eq!(
            session.backend.transaction_model(),
            StorageTransactionModel::VersionedConcurrent {
                database: request.target(),
            }
        );
        drop(session);
        let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
        assert_eq!(&observe(&engine, provider.as_ref()), self.expected);
        let sequence = &self.expected.sequences[0];
        let observed = sql(&engine, "SELECT last_value, is_called FROM archive.ticket");
        assert_eq!(observed[0]["last_value"], Value::Int(sequence.current));
        assert_eq!(observed[0]["is_called"], Value::Bool(sequence.called));
        assert!(engine.sql("SELECT * FROM private_temporary", &[]).is_err());
        assert!(engine
            .sql("SELECT * FROM archive.private_definition", &[])
            .is_err());
    }
}

impl BackupRestoreImporter for Importer<'_> {
    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let file = directory.create_file("database")?;
        if matches!(self.kind, Provider::Redb) {
            let mut import =
                RedbBackupImport::new(file, backup, request, 4 << 20, 16 << 20, control)?;
            while import.advance()? != RedbBackupImportStep::Complete {}
            drop(import);
        } else {
            drop(file);
            directory.with_file_path("database", control, |path| {
                match self.kind {
                    Provider::Native => {
                        let mut import = SQLiteNativeBackupImport::new(
                            path,
                            backup,
                            request,
                            self.options(),
                            control,
                        )?;
                        while import.advance()? != SQLiteBackupImportStep::Complete {}
                    }
                    Provider::KV => {
                        let mut import = SQLiteKeyValueBackupImport::new(
                            path,
                            backup,
                            request,
                            self.options(),
                            control,
                        )?;
                        while import.advance()? != SQLiteBackupImportStep::Complete {}
                    }
                    Provider::Redb => unreachable!(),
                }
                Ok(())
            })?;
        }
        directory.with_file_path("database", control, |path| {
            self.validate(path, request);
            Ok(())
        })
    }

    fn recover(
        self,
        _: &RestoreDirectory<'_>,
        _: &BackupLease<'_>,
        _: DatabaseRestore,
        _: &StorageReadControl,
    ) -> BackupResult<()> {
        panic!("fresh-target drill does not resume an interrupted import")
    }
}

pub(super) fn verify(
    repository: &LocalRepository,
    root: BackupRoot,
    target: &Path,
    kind: Provider,
    mode: usize,
    expected: &Observed,
    control: &StorageReadControl,
) {
    let mut operation = repository
        .begin_restore(root.backup(), target, control)
        .unwrap();
    let request = operation.request();
    assert_ne!(request.source(), request.target());
    operation
        .prepare(Importer {
            kind,
            mode,
            expected,
        })
        .unwrap();
    assert!(!target.exists(), "validation precedes target publication");
    let receipt = operation.publish().unwrap();
    assert_eq!(receipt.database, request.target());
    let path = target.join("database");
    {
        let provider = kind.open(&path, mode);
        let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
        assert_eq!(&observe(&engine, provider.as_ref()), expected);
        let previous = &expected.sequences[0];
        let next = previous.current
            + if previous.called {
                previous.increment
            } else {
                0
            };
        assert_eq!(
            sql(&engine, "SELECT nextval('archive.ticket') AS value")[0]["value"],
            Value::Int(next)
        );
        sql(
            &engine,
            "INSERT INTO archive.docs VALUES (99, 'later write', ARRAY[0.0, 1.0], 'done', 9)",
        );
        sql(
            &engine,
            "CREATE TABLE archive.after_restore (value INTEGER)",
        );
        sql(&engine, "INSERT INTO archive.after_restore VALUES (23)");
    }
    assert_eq!(operation.publish().unwrap(), receipt);
    drop(operation);
    let reopened = Engine::from_persistent_provider(kind.open(&path, mode)).unwrap();
    assert_eq!(
        sql(&reopened, "SELECT title FROM archive.docs WHERE id = 99")[0]["title"],
        Value::Str("later write".into())
    );
    assert_eq!(
        sql(&reopened, "SELECT value FROM archive.after_restore")[0]["value"],
        Value::Int(23)
    );
}
