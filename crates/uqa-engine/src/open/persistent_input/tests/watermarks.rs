//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::RelationIdentity;

fn fixture(provider: Provider, path: &std::path::Path) -> Engine {
    let source = provider.open(path);
    source.sql("CREATE TABLE root_a(id bigint) PARTITION BY RANGE(id); CREATE TABLE child_a PARTITION OF root_a DEFAULT; CREATE TABLE root_b(id bigint) PARTITION BY RANGE(id); CREATE TABLE child_b PARTITION OF root_b DEFAULT", &[]).unwrap();
    let catalog = source.storage.catalog.as_ref().unwrap();
    for mut row in catalog.load_tables().unwrap() {
        let mut columns: Vec<uqa_sql::ast::ColumnDef> =
            serde_json::from_str(&row.columns_json).unwrap();
        columns[0].auto_increment = Some(uqa_sql::ast::AutoIncrement::legacy());
        row.columns_json = serde_json::to_string(&columns).unwrap();
        catalog.save_table(&row).unwrap();
        let value = match row.relation.name.as_str() {
            "child_a" => "901",
            "child_b" => "1801",
            _ => "7",
        };
        catalog
            .set_metadata(
                &uqa_storage::document_store::identifiers::legacy_document_id_metadata_key(
                    &row.relation.qualified_name(),
                ),
                value,
            )
            .unwrap();
    }
    source
}

fn counter(engine: &Engine, name: &str) -> u128 {
    *engine.storage.tables.read()[&RelationIdentity::new("public", name)]
        .next_id
        .read()
}

fn reach_publication(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::WatermarkPublication(_)
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("watermark publication was not reached")
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_partition_watermarks_match_reopen_and_publish_one_root_per_step(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(provider, &directory.path().join("partitions"));
    let catalog = source.storage.catalog.as_ref().unwrap();
    let before = catalog.metadata_with_prefix("").unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_publication(&mut input);
    let engine = input.candidate.as_ref().unwrap().session.engine();
    assert_eq!(
        (counter(engine, "root_a"), counter(engine, "root_b")),
        (7, 7)
    );
    assert!(input.advance(1).unwrap().is_none());
    let engine = input.candidate.as_ref().unwrap().session.engine();
    assert_eq!(
        (counter(engine, "root_a"), counter(engine, "root_b")),
        (901, 7)
    );
    assert!(input.advance(1).unwrap().is_none());
    let engine = input.candidate.as_ref().unwrap().session.engine();
    assert_eq!(
        (counter(engine, "root_a"), counter(engine, "root_b")),
        (901, 1801)
    );
    let restored = finish(&mut input).finish().unwrap();
    assert_eq!(
        (counter(&restored, "root_a"), counter(&restored, "root_b")),
        (901, 1801)
    );
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
    let provider = source.storage.provider.as_ref().unwrap();
    let ordinary = Engine::from_initialized_persistent_session(
        provider.open_session().unwrap(),
        Some(Arc::clone(provider)),
    )
    .unwrap();
    assert_eq!(
        (counter(&ordinary, "root_a"), counter(&ordinary, "root_b")),
        (901, 1801)
    );
    // Initial opening can retire legacy metadata into the autonomous allocator. Its cached counter is not the allocator's durable watermark.
    let reopened = Engine::from_persistent_provider(Arc::clone(provider)).unwrap();
    let row = reopened
        .sql("INSERT INTO root_a DEFAULT VALUES RETURNING id", &[])
        .unwrap();
    assert_eq!(row.rows[0]["id"], Value::Int(901));
    let row = restored
        .sql("INSERT INTO root_a DEFAULT VALUES RETURNING id", &[])
        .unwrap();
    assert_eq!(row.rows[0]["id"], Value::Int(902));
    drop((restored, ordinary, reopened, input));
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_partition_watermarks_retire_partial_private_publication(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(provider, &directory.path().join("partitions"));
    let before = source
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .metadata_with_prefix("")
        .unwrap();
    for applied in [0, 1] {
        for terminal in ["cancel", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(32 << 20);
            let mut input = input(&source, &control);
            reach_publication(&mut input);
            let engine = input.candidate.as_ref().unwrap().session.engine();
            let backend = Arc::clone(engine.storage.backend.as_ref().unwrap());
            let table = Arc::downgrade(
                &engine.storage.tables.read()[&RelationIdentity::new("public", "root_a")],
            );
            for _ in 0..applied {
                assert!(input.advance(1).unwrap().is_none());
            }
            match terminal {
                "cancel" => {
                    control.cancellation().cancel();
                    assert!(input.advance(1).is_err());
                    assert!(input.candidate.is_none());
                }
                "zero" => {
                    assert!(input.advance(0).is_err());
                    assert!(input.candidate.is_none());
                }
                "drop" => (),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("retire partial counter publication");
                    }));
                    assert!(!backend.in_transaction());
                    assert!(table.upgrade().is_none());
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                _ => unreachable!(),
            }
            if matches!(terminal, "cancel" | "zero") {
                assert!(input.advance(1).is_err());
            }
            drop(input);
            assert!(!backend.in_transaction());
            assert!(table.upgrade().is_none());
            assert_eq!(control.memory().used(), 0);
        }
    }
    assert_eq!(
        source
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .metadata_with_prefix("")
            .unwrap(),
        before
    );
    source
        .sql(
            "CREATE TABLE after_retirement(id integer); INSERT INTO after_retirement VALUES(1)",
            &[],
        )
        .unwrap();
}
