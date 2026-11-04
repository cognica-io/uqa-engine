//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use uqa_core::memory::MemoryBudget;
use uqa_execution::catalog::comments::{snapshot::CommentCaptureSource, CommentRegistry};
use uqa_storage::memory_capture::MemoryCaptureSource;

fn descriptions(engine: &Engine, query: &str) -> Vec<String> {
    engine
        .sql(query, &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| {
            let Value::Str(text) = &row["description"] else {
                panic!("description text")
            };
            text.clone()
        })
        .collect()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_comment_lifetime_matches_postgresql_through_rollback_capture_and_reopen(
    #[case] provider: Provider,
) {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../../tests/parity/pg18/temporary_comments.expected.json"
    ))
    .unwrap();
    assert!(fixture["version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("comments.db");
    let engine = provider.open(&path);
    let mut source = None;
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["reopen"] != true)
    {
        let text = case["sql"].as_str().unwrap();
        if case["observe"] == true {
            assert_eq!(
                serde_json::json!(descriptions(&engine, text)),
                case["descriptions"],
                "{provider:?}: {}",
                case["id"]
            );
        } else {
            sql(&engine, text);
        }
        if case["id"] == "original" {
            source = Some(CommentCaptureSource::new(&engine.durable.comments.read()));
        }
    }
    let native = engine.durable.comments.read().budget().clone();
    {
        let registry = engine.durable.comments.read();
        assert_eq!(registry.len(), 13);
        for (address, text) in registry.iter() {
            let durable = text.starts_with("backup-durable") || text == "backup-unlogged";
            assert_eq!(
                registry.is_temporary(address),
                !durable,
                "{provider:?}: {text}"
            );
        }
    }
    if let Some(catalog) = &engine.storage.catalog {
        let mut stored = catalog
            .metadata_with_prefix("uqa.sql.comment.v1:")
            .unwrap()
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>();
        stored.sort();
        assert_eq!(stored, ["backup-durable-updated", "backup-unlogged"]);
    }
    drop(engine);
    let control = StorageReadControl::with_limit(64 << 10);
    let source = source.unwrap();
    let captured = source.freeze(&control).unwrap();
    drop(source);
    let mut saved = captured
        .get()
        .unwrap()
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>();
    saved.sort_unstable();
    assert_eq!(saved, ["backup-durable", "backup-unlogged"]);
    assert!(native.used() > 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        for case in fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["reopen"] == true)
        {
            assert_eq!(
                serde_json::json!(descriptions(&reopened, case["sql"].as_str().unwrap())),
                case["descriptions"],
                "{provider:?}: reopen"
            );
        }
    }
    drop(captured);
    assert_eq!(native.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_comment_denial_preserves_durable_and_temporary_roots_and_later_use(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("comments.db"));
    sql(&engine, "CREATE TYPE saved AS ENUM ('one'); COMMENT ON TYPE saved IS 'durable'; CREATE TYPE pg_temp.local_type AS ENUM ('one'); COMMENT ON TYPE pg_temp.local_type IS 'temporary'");
    let memory = MemoryBudget::new(1 << 20);
    let mut admitted = CommentRegistry::with_budget(&memory);
    {
        let current = engine.durable.comments.read();
        for (address, text) in current.iter() {
            let persistence = if current.is_temporary(address) {
                RelationPersistence::Temporary
            } else {
                RelationPersistence::Permanent
            };
            admitted.insert(*address, text, persistence).unwrap();
        }
    }
    *engine.durable.comments.write() = admitted;
    let original = engine.durable.comments.snapshot();
    let stored = engine
        .storage
        .catalog
        .as_ref()
        .map(|catalog| catalog.metadata_with_prefix("uqa.sql.comment.v1:").unwrap());
    for (target, explicit) in [
        ("saved", false),
        ("pg_temp.local_type", false),
        ("saved", true),
        ("pg_temp.local_type", true),
    ] {
        if explicit {
            sql(&engine, "BEGIN");
        }
        let occupied = memory.reserve(memory.available()).unwrap();
        let error = engine
            .sql(&format!("COMMENT ON TYPE {target} IS 'denied'"), &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
        if explicit {
            sql(&engine, "ROLLBACK");
        }
        assert_eq!(*engine.durable.comments.read(), *original);
        if let Some(catalog) = &engine.storage.catalog {
            assert_eq!(
                &catalog.metadata_with_prefix("uqa.sql.comment.v1:").unwrap(),
                stored.as_ref().unwrap()
            );
        }
        drop(occupied);
    }
    sql(&engine, "COMMENT ON TYPE pg_temp.local_type IS 'usable'; COMMENT ON TYPE saved IS 'usable'; DROP TYPE pg_temp.local_type; DROP TYPE saved");
    assert!(engine.durable.comments.read().is_empty());
    drop((engine, original));
    assert_eq!(memory.used(), 0);
}
