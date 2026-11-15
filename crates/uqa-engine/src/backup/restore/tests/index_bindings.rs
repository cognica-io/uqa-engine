//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use std::{path::Path, sync::Arc};
use uqa_core::{catalog_index::CatalogIndexRow, RelationIdentity};
use uqa_sql::{
    ast::{Expr, IndexKey},
    catalog::index::IndexDefinition,
    ColumnType,
};
use uqa_storage::PersistentStorageProvider;

mod typed_rows;

fn destination(provider: Provider, path: &Path) -> RestoreDestination {
    use uqa_storage_sqlite::{SQLiteBackupMapping, SQLiteBackupTarget};
    let target: Arc<dyn PersistentBackupTarget> = match provider {
        Provider::Memory => return RestoreDestination::Memory,
        Provider::SQLiteNative => Arc::new(SQLiteBackupTarget::new(SQLiteBackupMapping::Native)),
        Provider::SQLiteKeyValue => {
            Arc::new(SQLiteBackupTarget::new(SQLiteBackupMapping::KeyValue))
        }
        Provider::Redb => Arc::new(uqa_storage_redb::RedbBackupTarget::default()),
    };
    RestoreDestination::Persistent {
        directory: path.to_path_buf(),
        target,
    }
}

fn fixture(provider: Provider, path: &Path) -> Engine {
    let engine = provider.open(path);
    engine.sql("CREATE TABLE items(id integer, label text); CREATE FUNCTION identity_value(x integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS $$ SELECT x $$; CREATE UNIQUE INDEX saved ON items(identity_value(id)) INCLUDE(label,label) WHERE id > 0; INSERT INTO items VALUES(1,'one'),(-1,'negative'); ALTER FUNCTION identity_value(integer) RENAME TO renamed_identity", &[]).unwrap();
    engine.sql("CREATE TYPE mood AS ENUM ('low','high'); CREATE TABLE typed(mood mood); CREATE INDEX typed_saved ON typed(mood); ALTER TYPE mood RENAME TO renamed_mood", &[]).unwrap();
    engine
}

fn corrupt(row: &mut CatalogIndexRow, kind: &str) {
    let mut keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json).unwrap();
    let mut definition: IndexDefinition =
        serde_json::from_str(row.definition_json.as_deref().unwrap()).unwrap();
    match kind {
        "key_column" => {
            let IndexKey::Expression(expression) = &mut keys[0] else {
                panic!("expression key")
            };
            let Expr::Func { args, .. } = expression.as_mut() else {
                panic!("routine key")
            };
            args[0] = Expr::Column("absent".into());
        }
        "routine_identity" => {
            let IndexKey::Expression(expression) = &mut keys[0] else {
                panic!("expression key")
            };
            let Expr::Func {
                binding: Some(binding),
                ..
            } = expression.as_mut()
            else {
                panic!("bound routine")
            };
            binding.object_id = Some([255; 16]);
        }
        "result_type" => definition.key_types[0] = ColumnType::Text,
        "type_count" => definition.key_types.push(ColumnType::Text),
        "type_identity" => {
            let ColumnType::Enum(reference) = &mut definition.key_types[0] else {
                panic!("enum key")
            };
            reference.array_oid += 1;
        }
        "predicate_column" => {
            definition.predicate = Some(Box::new(Expr::IsNull {
                expr: Box::new(Expr::Column("absent".into())),
                negated: false,
            }));
        }
        "predicate_type" => definition.predicate = Some(Box::new(Expr::Column("id".into()))),
        "predicate_child" => {
            definition.predicate = Some(Box::new(Expr::And(vec![
                Expr::Literal(Value::Bool(true)),
                Expr::Column("id".into()),
            ])));
        }
        _ => unreachable!(),
    }
    row.columns_json = serde_json::to_string(&keys).unwrap();
    row.definition_json = Some(serde_json::to_string(&definition).unwrap());
}

fn capture_provider(
    provider: &dyn PersistentStorageProvider,
    repository: &LocalRepository,
) -> BackupRoot {
    let control = StorageReadControl::with_limit(32 << 20);
    let image = provider.capture_backup_image(&control).unwrap();
    let mut job = repository
        .begin_image_backup(image, None, 32 << 20, 16 << 20, &control)
        .unwrap();
    while job.advance(8, 4096).unwrap() == BackupCaptureStep::Pending {}
    let root = job.publish().unwrap();
    drop(job);
    assert_eq!(control.memory().used(), 0);
    root
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_index_bindings_reject_corrupt_archives_before_activation(#[case] provider: Provider) {
    for corruption in [
        "key_column",
        "routine_identity",
        "result_type",
        "type_count",
        "type_identity",
        "predicate_column",
        "predicate_type",
        "predicate_child",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = fixture(provider, &directory.path().join("source"));
        let name = RelationIdentity::new(
            "public",
            if corruption == "type_identity" {
                "typed_saved"
            } else {
                "saved"
            },
        );
        let mut row = source
            .durable
            .catalog_indexes
            .read()
            .get(&name)
            .unwrap()
            .clone();
        corrupt(&mut row, corruption);
        let root = if let Some(persistent) = source.storage.provider.clone() {
            drop(source);
            let session = persistent.open_session().unwrap();
            session.catalog.save_catalog_index_row(&row).unwrap();
            drop(session);
            capture_provider(persistent.as_ref(), &repository)
        } else {
            source.durable.catalog_indexes.write().replace(row).unwrap();
            let root = capture(&source, &repository, None);
            drop(source);
            root
        };
        let path = directory.path().join("restored");
        let control = StorageReadControl::with_limit(32 << 20);
        let authority = Engine::new();
        let mut job = authority
            .begin_restore(
                &repository,
                root.backup(),
                destination(provider, &path),
                RestoreOptions::new(16 << 20, 8 << 20),
                &control,
            )
            .unwrap();
        let mut failure = None;
        for _ in 0..65536 {
            match job.advance(1024) {
                Err(error) => {
                    failure = Some(error);
                    break;
                }
                Ok(RestorePhase::Reading | RestorePhase::Validating) => (),
                Ok(phase) => panic!("{provider:?} {corruption}: invalid index reached {phase:?}"),
            }
        }
        let error = failure.expect("index validation must finish");
        assert!(
            error.to_string().contains("native index")
                || error.to_string().contains("native routine")
                || error.to_string().contains("native type")
                || error.to_string().contains("absent")
                || error.to_string().contains("boolean"),
            "{provider:?} {corruption}: {error}"
        );
        assert_eq!(job.phase(), RestorePhase::Failed);
        assert!(!path.exists());
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(job.publish(), Err(BackupError::Finished)));
        drop(job);
        repository.remove_backup(root.backup(), &control).unwrap();
        assert_eq!(control.memory().used(), 0);
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_index_bindings_preserve_renamed_routines_and_later_writes(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = fixture(provider, &directory.path().join("source"));
    source
        .sql("ALTER FUNCTION renamed_identity(integer) VOLATILE", &[])
        .unwrap();
    let root = capture(&source, &repository, None);
    let rows = source
        .sql("SELECT id,label FROM items ORDER BY id", &[])
        .unwrap()
        .rows;
    let indexes = source
        .durable
        .catalog_indexes
        .read()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    drop(source);
    let path = directory.path().join("restored");
    let control = StorageReadControl::with_limit(32 << 20);
    let authority = Engine::new();
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &path),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let restored = job.publish().unwrap();
    assert_eq!(
        restored
            .sql("SELECT id,label FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        rows
    );
    assert_eq!(
        format!(
            "{:?}",
            restored
                .durable
                .catalog_indexes
                .read()
                .values()
                .cloned()
                .collect::<Vec<_>>()
        ),
        format!("{indexes:?}")
    );
    assert!(restored
        .sql("INSERT INTO items VALUES(1,'duplicate')", &[])
        .is_err());
    restored
        .sql(
            "INSERT INTO items VALUES(2,'two'),(-1,'outside predicate')",
            &[],
        )
        .unwrap();
    assert_eq!(
        restored
            .sql(
                "SELECT label FROM items WHERE renamed_identity(id)=2 AND id>0",
                &[]
            )
            .unwrap()
            .rows[0]["label"],
        Value::Str("two".into())
    );
    drop((restored, job));
    assert_eq!(control.memory().used(), 0);
}
