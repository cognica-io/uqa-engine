//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::{capture_provider, destination};
use super::*;
use crate::tests::providers::Provider;
use std::path::Path;
use uqa_core::RelationIdentity;
use uqa_execution::catalog::foreign::ForeignServerReference;

#[derive(Clone, Copy, Debug)]
enum Binding {
    WrongOid,
    WrongName,
    Historical,
}

fn archive(
    provider: Provider,
    path: &Path,
    repository: &LocalRepository,
    binding: Binding,
) -> (BackupRoot, u32) {
    let source = provider.open(path);
    source
        .sql("CREATE SERVER saved FOREIGN DATA WRAPPER memory_fdw", &[])
        .unwrap();
    let historical = ForeignServerReference::from(&source.durable.foreign_servers.read()["saved"]);
    source.sql("DROP SERVER saved; CREATE SERVER saved FOREIGN DATA WRAPPER memory_fdw; CREATE SERVER other FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE target(id integer) SERVER saved; CREATE FOREIGN TABLE live_target(id integer) SERVER saved", &[]).unwrap();
    let live = ForeignServerReference::from(&source.durable.foreign_servers.read()["saved"]);
    assert_ne!(historical.object_id, live.object_id);
    assert_ne!(historical.oid, live.oid);
    let (name, reference) = match binding {
        Binding::WrongOid => (
            "saved",
            ForeignServerReference {
                oid: historical.oid,
                ..live
            },
        ),
        Binding::WrongName => ("other", live),
        Binding::Historical => ("saved", historical),
    };
    let target = RelationIdentity::new("public", "target");
    let root = if let Some(persistent) = source.storage.provider.clone() {
        drop(source);
        let session = persistent.open_session().unwrap();
        let mut row = session
            .catalog
            .load_foreign_tables()
            .unwrap()
            .into_iter()
            .find(|row| row.relation == target)
            .unwrap();
        let mut schema: serde_json::Value = serde_json::from_str(&row.columns_json).unwrap();
        schema["server_reference"] = serde_json::to_value(reference).unwrap();
        row.columns_json = schema.to_string();
        row.server_name = name.into();
        session.catalog.save_foreign_table(&row).unwrap();
        drop(session);
        capture_provider(persistent.as_ref(), repository)
    } else {
        let mut catalog = source.durable.foreign_catalog.write();
        let mut table = catalog.tables().get(&target).unwrap().clone();
        table.server_reference = Some(reference);
        table.server_name = name.into();
        catalog.replace_table(target, table).unwrap();
        drop(catalog);
        capture(&source, repository, None)
    };
    (root, historical.oid)
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restore_rejects_a_live_foreign_server_bound_to_a_different_oid_or_name(
    #[case] provider: Provider,
    #[values(Binding::WrongOid, Binding::WrongName)] binding: Binding,
) {
    let files = tempfile::tempdir().unwrap();
    let repository = repository(&files.path().join("repository"));
    let (root, _) = archive(provider, &files.path().join("source"), &repository, binding);
    let target = files.path().join("restored");
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = Engine::new()
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &target),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    let error = loop {
        match job.advance(4096) {
            Err(error) => break error,
            Ok(RestorePhase::Reading | RestorePhase::Validating) => (),
            Ok(phase) => panic!("{provider:?}: inconsistent foreign server reached {phase:?}"),
        }
    };
    assert!(error.to_string().contains("foreign server"), "{error}");
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    assert!(!target.exists());
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restore_preserves_live_and_historical_foreign_server_bindings(#[case] provider: Provider) {
    let files = tempfile::tempdir().unwrap();
    let repository = repository(&files.path().join("repository"));
    let (root, historical_oid) = archive(
        provider,
        &files.path().join("source"),
        &repository,
        Binding::Historical,
    );
    let target = files.path().join("restored");
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = Engine::new()
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &target),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let restored = job.publish().unwrap();
    drop(job);
    restored
        .sql("EXPLAIN SELECT * FROM live_target", &[])
        .unwrap();
    let error = restored
        .sql("EXPLAIN SELECT * FROM target", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(
        error.to_string(),
        format!("cache lookup failed for foreign server {historical_oid}")
    );
    restored.sql("DROP FOREIGN TABLE target", &[]).unwrap();
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}
