//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::{capture_provider, destination};
use super::*;
use crate::tests::providers::Provider;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{ColumnDef, Expr};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_constraint_bindings_reject_non_boolean_checks_before_activation(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    source
        .sql("CREATE TABLE items(id integer CHECK(id > 0))", &[])
        .unwrap();
    let name = RelationIdentity::new("public", "items");
    let root = if let Some(persistent) = source.storage.provider.clone() {
        drop(source);
        let session = persistent.open_session().unwrap();
        let mut table = session
            .catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|table| table.relation == name)
            .unwrap();
        let mut columns: Vec<ColumnDef> = serde_json::from_str(&table.columns_json).unwrap();
        columns[0].check = Some(Expr::Column("id".into()));
        table.columns_json = serde_json::to_string(&columns).unwrap();
        session.catalog.save_table(&table).unwrap();
        drop(session);
        capture_provider(persistent.as_ref(), &repository)
    } else {
        source
            .storage
            .tables
            .read()
            .get(&name)
            .unwrap()
            .columns
            .write()
            .edit(|columns| {
                columns[0].check = Some(Expr::Column("id".into()));
                Ok(())
            })
            .unwrap();
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
            Ok(phase) => panic!("{provider:?}: non-boolean CHECK reached {phase:?}"),
        }
    }
    let error = failure.expect("constraint validation must finish");
    assert!(
        error.to_string().contains("native constraint") || error.to_string().contains("boolean"),
        "{provider:?}: {error}"
    );
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(!path.exists());
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
}
