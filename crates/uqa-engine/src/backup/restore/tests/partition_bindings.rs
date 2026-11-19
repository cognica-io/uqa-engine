//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::{capture_provider, destination};
use super::*;
use crate::tests::providers::Provider;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{Expr, PartitionBound, TableConstraintSet};

mod corruption;
mod preserved;
mod range_keys;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_partition_bounds_reject_mistyped_constants_before_activation(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    source
        .sql("CREATE TABLE parent(id integer) PARTITION BY LIST(id); CREATE TABLE child PARTITION OF parent FOR VALUES IN (1)", &[])
        .unwrap();
    let name = RelationIdentity::new("public", "child");
    let invalid = PartitionBound::List(vec![Expr::Literal(Value::Bool(true))]);
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
        let mut constraints: TableConstraintSet =
            serde_json::from_str(&table.constraints_json).unwrap();
        constraints.hierarchy.partition_bound = Some(invalid);
        table.constraints_json = serde_json::to_string(&constraints).unwrap();
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
            .hierarchy
            .write()
            .edit(|hierarchy| {
                hierarchy.partition_bound = Some(invalid);
                Ok(())
            })
            .unwrap();
        let root = capture(&source, &repository, None);
        drop(source);
        root
    };
    reject_archive(
        provider,
        &repository,
        root,
        &directory.path().join("restored"),
        "partition",
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn partition_key_types_require_original_ordering_support(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("source"));
    engine
        .sql("CREATE DOMAIN unsupported_key AS json", &[])
        .unwrap();
    // The PostgreSQL reference fixture independently rejects these six declarations with 42704.
    for ty in ["json", "xml", "unsupported_key"] {
        for strategy in ["LIST", "RANGE"] {
            let sql = format!("CREATE TABLE rejected_key(id {ty}) PARTITION BY {strategy}(id)");
            let error = engine.sql(&sql, &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("42704"), "{provider:?}: {sql}");
            assert!(
                error.to_string().contains("has no default operator class"),
                "{error}"
            );
        }
    }
    for (keys, state) in [("id,(random()+n)", "42704"), ("(random()+n),id", "42P17")] {
        let sql =
            format!("CREATE TABLE rejected_key(id json,n integer) PARTITION BY RANGE({keys})");
        assert_eq!(
            engine.sql(&sql, &[]).unwrap_err().sqlstate(),
            Some(state),
            "{sql}"
        );
    }
    engine
        .sql(
            "CREATE TABLE rejected_key(id integer) PARTITION BY LIST(id)",
            &[],
        )
        .unwrap();
}

fn reject_archive(
    provider: Provider,
    repository: &LocalRepository,
    root: BackupRoot,
    path: &std::path::Path,
    expected: &str,
) {
    let control = StorageReadControl::with_limit(32 << 20);
    let authority = Engine::new();
    let mut job = authority
        .begin_restore(
            repository,
            root.backup(),
            destination(provider, path),
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
            Ok(phase) => panic!("{provider:?}: invalid partition reached {phase:?}"),
        }
    }
    let error = failure.expect("partition validation must finish");
    assert!(
        error.to_string().contains(expected),
        "{provider:?}: {error}"
    );
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(!path.exists());
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
}
