//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh-target drills validate Engine observations while the repository target is still private.

use super::*;
use uqa_storage::{
    backup::{
        BackupLease, BackupRestoreFactory, BackupRestoreStep, BackupRestoreWork,
        BackupRestoreWorkStep, RestoreDirectory, RestoreFileWork,
    },
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

enum Input<'a> {
    Native(Box<SQLiteNativeBackupImport<'a>>),
    KV(Box<SQLiteKeyValueBackupImport<'a>>),
    Redb(Box<RedbBackupImport<'a>>),
}

impl Input<'_> {
    fn records(&self) -> u64 {
        match self {
            Self::Native(input) => input.imported_records() + input.imported_identifiers(),
            Self::KV(input) => input.imported_records() + input.imported_identifiers(),
            Self::Redb(input) => input.imported_records() + input.imported_identifiers(),
        }
    }

    fn advance(&mut self) -> BackupResult<bool> {
        let before = self.records();
        let complete = match self {
            Self::Native(input) => input.advance()? == SQLiteBackupImportStep::Complete,
            Self::KV(input) => input.advance()? == SQLiteBackupImportStep::Complete,
            Self::Redb(input) => input.advance()? == RedbBackupImportStep::Complete,
        };
        assert!(self.records() - before <= 1);
        Ok(complete)
    }
}

struct Work<'a, 'expected> {
    input: Option<Input<'a>>,
    validator: Importer<'expected>,
    path: std::path::PathBuf,
    request: DatabaseRestore,
}

impl BackupRestoreWork for Work<'_, '_> {
    fn advance(&mut self) -> BackupResult<BackupRestoreWorkStep> {
        if let Some(input) = &mut self.input {
            if input.advance()? {
                self.input = None;
            }
            return Ok(BackupRestoreWorkStep::Pending);
        }
        // This fixed-fixture observer is not the production logical-image validator.
        self.validator.validate(&self.path, self.request);
        Ok(BackupRestoreWorkStep::Complete)
    }
}

impl<'a, 'expected> BackupRestoreFactory<'a> for Importer<'expected> {
    type Work = RestoreFileWork<Work<'a, 'expected>>;

    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Self::Work> {
        let file = directory.create_file("database")?;
        directory.begin_file_work("database", control, |path| {
            let input = match self.kind {
                Provider::Redb => Input::Redb(Box::new(RedbBackupImport::new(
                    file,
                    backup,
                    request,
                    4 << 20,
                    16 << 20,
                    control,
                )?)),
                Provider::Native => {
                    drop(file);
                    Input::Native(Box::new(SQLiteNativeBackupImport::new(
                        path,
                        backup,
                        request,
                        self.options(),
                        control,
                    )?))
                }
                Provider::KV => {
                    drop(file);
                    Input::KV(Box::new(SQLiteKeyValueBackupImport::new(
                        path,
                        backup,
                        request,
                        self.options(),
                        control,
                    )?))
                }
            };
            Ok(Work {
                input: Some(input),
                validator: self,
                path: path.to_owned(),
                request,
            })
        })
    }

    fn recover(
        self,
        _: &RestoreDirectory<'_>,
        _: BackupLease<'a>,
        _: DatabaseRestore,
        _: &StorageReadControl,
    ) -> BackupResult<Self::Work> {
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
    let operation = repository
        .begin_restore(root.backup(), target, control)
        .unwrap();
    let request = operation.request();
    assert_ne!(request.source(), request.target());
    let mut job = operation
        .into_job(Importer {
            kind,
            mode,
            expected,
        })
        .unwrap();
    let mut steps = 0;
    while job.advance().unwrap() != BackupRestoreStep::Prepared {
        steps += 1;
        assert!(steps < 100_000);
        assert!(!target.exists(), "partial import remains private");
    }
    assert!(steps > 1);
    assert!(!target.exists(), "validation precedes target publication");
    let receipt = job.publish().unwrap();
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
    assert_eq!(job.advance().unwrap(), BackupRestoreStep::Complete);
    assert_eq!(job.publish().unwrap(), receipt);
    drop(job);
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
