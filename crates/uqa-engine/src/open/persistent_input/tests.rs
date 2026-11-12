//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use uqa_core::Value;

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_role_restore_keeps_the_original_snapshot_and_accepts_later_authority_changes(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("roles"));
    source.sql("CREATE ROLE reader; CREATE ROLE team; GRANT team TO reader; CREATE TABLE items(id integer); INSERT INTO items VALUES(7); GRANT SELECT ON items TO team", &[]).unwrap();
    let original_roles = source.durable.roles.read().clone();
    let original_members = source.durable.role_memberships.read().clone();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    assert!(input.advance(1).unwrap().is_none());
    assert!(matches!(
        input.candidate.as_ref().unwrap().stage,
        Stage::Roles(_)
    ));
    assert!(input.advance(3).unwrap().is_none());
    source
        .sql("ALTER ROLE team RENAME TO team_later", &[])
        .unwrap();
    let session = finish(&mut input);
    assert_eq!(*session.engine().durable.roles.read(), original_roles);
    assert_eq!(
        *session.engine().durable.role_memberships.read(),
        original_members
    );
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.sql("SET ROLE reader", &[]).unwrap();
    assert_eq!(
        restored.sql("SELECT id FROM items", &[]).unwrap().rows[0]["id"],
        Value::Int(7)
    );
    restored
        .sql(
            "RESET ROLE; CREATE ROLE another; GRANT team_later TO another",
            &[],
        )
        .unwrap();
    drop((restored, input, source, original_roles, original_members));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

fn input(source: &Engine, control: &StorageReadControl) -> PersistentEngineInput {
    PersistentEngineInput::new(
        Arc::clone(source.storage.provider.as_ref().unwrap()),
        MemoryBudget::new(16 << 20),
        LIMITS,
        control,
    )
    .unwrap()
}

fn finish(input: &mut PersistentEngineInput) -> PersistentEngineState {
    for _ in 0..65536 {
        if let Some(session) = input.advance(1).unwrap() {
            return session;
        }
    }
    panic!("persistent Engine input stopped progressing");
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_engine_input_retains_one_snapshot_through_catalog_validation(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("source"));
    source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text); INSERT INTO items VALUES(1,'original')", &[]).unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    let backend = Arc::clone(
        input
            .candidate
            .as_ref()
            .unwrap()
            .session
            .engine
            .storage
            .backend
            .as_ref()
            .unwrap(),
    );
    let revision = backend.read_view_revision().unwrap();
    assert!(backend.in_transaction());
    assert!(input.advance(1).unwrap().is_none());
    source.sql("CREATE TABLE later(value integer); INSERT INTO items VALUES(2,'later'); INSERT INTO later VALUES(42)", &[]).unwrap();
    let session = finish(&mut input);
    assert!(backend.read_view_revision().unwrap() == revision);
    assert!(backend.in_transaction());
    {
        let tables = session.engine().storage.tables.read();
        assert_eq!(tables.len(), 1);
        let table = &tables[&uqa_core::RelationIdentity::new("public", "items")];
        assert_eq!(table.document_store.read().max_doc_id().unwrap(), 1);
    }
    let production = uqa_core::memory::ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    {
        let catalog = session
            .engine()
            .restored_catalog_read_view_with_control(&production)
            .unwrap();
        uqa_execution::catalog::image_validation::validate_namespaces(&catalog, &production)
            .unwrap();
    }
    assert!(backend.read_view_revision().unwrap() == revision);
    let restored = session.finish().unwrap();
    assert!(!backend.in_transaction());
    assert_eq!(
        restored.sql("SELECT value FROM later", &[]).unwrap().rows[0]["value"],
        Value::Int(42)
    );
    assert_eq!(
        restored
            .sql("SELECT count(*) AS n FROM items", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
    drop((restored, input, backend, source));
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_engine_input_retires_transaction_and_prefix_on_terminal_failure(
    #[case] provider: Provider,
    #[values("cancel", "memory", "zero", "drop")] outcome: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("source"));
    for table in 0..8 {
        source
            .sql(
                &format!("CREATE TABLE table_{table}(value integer, body text DEFAULT 'kept')"),
                &[],
            )
            .unwrap();
    }
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    let backend = Arc::clone(
        input
            .candidate
            .as_ref()
            .unwrap()
            .session
            .engine
            .storage
            .backend
            .as_ref()
            .unwrap(),
    );
    for _ in 0..8 {
        assert!(input.advance(1).unwrap().is_none());
    }
    assert!(backend.in_transaction());
    let held = if outcome == "memory" {
        Some(
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap(),
        )
    } else {
        None
    };
    match outcome {
        "drop" => (),
        "zero" => assert!(input.advance(0).is_err()),
        "cancel" => {
            control.cancellation().cancel();
            assert!(input.advance(1).is_err());
        }
        "memory" => {
            let mut denied = false;
            for _ in 0..65536 {
                match input.advance(1) {
                    Err(_) => {
                        denied = true;
                        break;
                    }
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("full memory admission was bypassed"),
                }
            }
            assert!(denied);
        }
        _ => unreachable!(),
    }
    if outcome != "drop" {
        assert!(!backend.in_transaction());
        assert!(input.advance(1).is_err());
    }
    drop((input, held));
    assert!(!backend.in_transaction());
    assert_eq!(control.memory().used(), 0);
    source
        .sql("INSERT INTO table_0 VALUES(3,'after retired input')", &[])
        .unwrap();
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_engine_input_abandoning_completed_catalog_releases_its_snapshot(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("source"));
    source
        .sql("CREATE TABLE items(value integer)", &[])
        .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    let session = finish(&mut input);
    let backend = Arc::clone(session.engine().storage.backend.as_ref().unwrap());
    assert!(backend.in_transaction());
    control.cancellation().cancel();
    drop((session, input));
    assert!(!backend.in_transaction());
    assert_eq!(control.memory().used(), 0);
}
