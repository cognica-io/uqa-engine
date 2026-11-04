//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::memory::MemoryBudget;
use uqa_storage::analyzer_catalog::AnalyzerCatalog;

const WORDS: &str = r#"{"tokenizer":{"type":"whitespace"}}"#;
const WHOLE: &str = r#"{"tokenizer":{"type":"keyword"}}"#;

fn bounded(engine: &Engine) -> MemoryBudget {
    let memory = MemoryBudget::new(1 << 20);
    let current = engine.durable.named_analyzers.snapshot();
    let mut next = AnalyzerCatalog::new(&memory);
    for name in current.keys() {
        next.insert(name, current.get(name).unwrap().clone())
            .unwrap();
    }
    *engine.durable.named_analyzers.write() = next;
    memory
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_analyzer_admission_precedes_catalog_writes_and_preserves_rollback(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native-analyzers.db");
    let engine = provider.open(&path);
    for index in 0..16 {
        engine
            .register_named_analyzer(&format!("saved{index:02}"), WORDS)
            .unwrap();
    }
    let stored = || {
        engine.storage.catalog.as_ref().map(|catalog| {
            (
                catalog.load_analyzers().unwrap(),
                catalog.load_analyzer_descriptors().unwrap(),
            )
        })
    };
    let expected = stored();
    for operation in 0..3 {
        engine.sql("BEGIN", &[]).unwrap();
        let memory = bounded(&engine);
        let original = engine.durable.named_analyzers.snapshot();
        let occupied = memory.reserve(memory.available()).unwrap();
        let result = match operation {
            0 => engine.register_named_analyzer("added", WHOLE),
            1 => engine.register_named_analyzer("saved00", WHOLE),
            _ => engine.drop_named_analyzer("saved00").map(|_| ()),
        };
        let error = result.unwrap_err();
        assert!(
            error.contains("memory"),
            "{provider:?}: {operation}: {error}"
        );
        let current = engine.durable.named_analyzers.snapshot();
        assert!(current.keys().eq(original.keys()));
        for name in original.keys() {
            assert!(std::sync::Arc::ptr_eq(
                current.get(name).unwrap(),
                original.get(name).unwrap()
            ));
        }
        assert_eq!(stored(), expected, "{provider:?}: {operation}");
        drop((occupied, current, original));
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(memory.used(), 0);
        assert_eq!(engine.transaction_depth(), 0);
    }
    engine.register_named_analyzer("saved00", WHOLE).unwrap();
    assert_eq!(
        engine
            .resolve_analyzer_revision("saved00")
            .unwrap()
            .analyze("two words")
            .unwrap(),
        ["two words"]
    );
    assert!(engine.drop_named_analyzer("saved01").unwrap());
    assert_eq!(engine.list_named_analyzers().unwrap().len(), 15);
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_eq!(reopened.list_named_analyzers().unwrap().len(), 15);
        assert_eq!(
            reopened
                .resolve_analyzer_revision("saved00")
                .unwrap()
                .analyze("two words")
                .unwrap(),
            ["two words"]
        );
    }
}

fn bounded_fields(engine: &Engine) -> MemoryBudget {
    let memory = MemoryBudget::new(1 << 20);
    let mut next = uqa_storage::analyzer_binding::registry::FieldAnalyzerCatalog::new(&memory);
    for (key, binding) in engine.durable.table_field_analyzers.read().iter() {
        let temporary = engine.try_table(&key.0).unwrap().unwrap().persistence
            == uqa_sql::ast::RelationPersistence::Temporary;
        next.insert(
            key,
            binding.clone(),
            temporary,
            &uqa_core::CancellationToken::new(),
        )
        .unwrap();
    }
    *engine.durable.table_field_analyzers.write() = next;
    memory
}

fn populate_fields(engine: &Engine) {
    engine.register_named_analyzer("saved", WORDS).unwrap();
    engine.register_named_analyzer("changed", WHOLE).unwrap();
    for index in 0..16 {
        engine.sql(&format!("CREATE TABLE docs{index:02}(body text, extra text); INSERT INTO docs{index:02} VALUES ('two words', 'other words'); CREATE INDEX body{index:02} ON docs{index:02} USING gin(body)"), &[]).unwrap();
        engine
            .set_table_field_analyzer(&format!("docs{index:02}"), "body", "saved", "both")
            .unwrap();
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_field_admission_preserves_bindings_postings_and_schema_recovery(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native-fields.db");
    let engine = provider.open(&path);
    populate_fields(&engine);
    let statements = [
        "CREATE INDEX extra00 ON docs00 USING gin(extra)",
        "ALTER TABLE docs00 RENAME COLUMN body TO renamed",
        "ALTER TABLE docs00 RENAME TO moved",
        "DROP INDEX body00",
        "ALTER TABLE docs00 DROP COLUMN body",
        "DROP TABLE docs00",
    ];
    for operation in 0..=statements.len() {
        engine.sql("BEGIN", &[]).unwrap();
        let memory = bounded_fields(&engine);
        let original = engine.durable.table_field_analyzers.snapshot();
        let stored = engine
            .storage
            .catalog
            .as_ref()
            .map(|catalog| catalog.load_table_field_analyzer_bindings().unwrap());
        let occupied = memory.reserve(memory.available()).unwrap();
        let result = if operation == 0 {
            engine.set_table_field_analyzer("docs00", "body", "changed", "both")
        } else {
            engine
                .sql(statements[operation - 1], &[])
                .map(|_| ())
                .map_err(|error| error.to_string())
        };
        let error = result.unwrap_err();
        assert!(
            error.contains("memory"),
            "{provider:?}, {operation}: {error}"
        );
        drop(occupied);
        engine.sql("ROLLBACK", &[]).unwrap();
        let current = engine.durable.table_field_analyzers.read();
        assert!(current
            .iter()
            .map(|(key, _)| key)
            .eq(original.iter().map(|(key, _)| key)));
        for (key, previous) in original.iter() {
            let now = current.get(key).unwrap();
            assert_eq!(now.to_json().unwrap(), previous.to_json().unwrap());
        }
        assert_eq!(
            engine
                .storage
                .catalog
                .as_ref()
                .map(|catalog| catalog.load_table_field_analyzer_bindings().unwrap()),
            stored
        );
        drop((original, current));
        assert_eq!(memory.used(), 0);
        let table = engine.try_table("docs00").unwrap().unwrap();
        assert!(table
            .columns
            .read()
            .iter()
            .any(|column| column.name == "body"));
        assert!(table.fts_fields().contains(&"body".to_owned()));
        let compiled = table
            .inverted_index
            .read()
            .index_analyzer_revision("body")
            .unwrap();
        assert_eq!(compiled.analyze("two words").unwrap(), ["two", "words"]);
        assert_eq!(engine.transaction_depth(), 0);
        assert_eq!(
            engine
                .sql("SELECT body FROM docs00 WHERE text_match(body, 'two')", &[])
                .unwrap()
                .rows,
            vec![std::collections::BTreeMap::from([(
                "body".into(),
                uqa_core::Value::Str("two words".into())
            )])]
        );
    }
    engine
        .set_table_field_analyzer("docs00", "body", "changed", "both")
        .unwrap();
    engine
        .sql(
            "ALTER TABLE docs00 RENAME COLUMN body TO renamed; ALTER TABLE docs00 RENAME TO moved",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .table_field_analyzer("moved", "renamed")
            .unwrap()
            .unwrap()
            .0,
        "changed"
    );
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        assert_eq!(
            engine
                .table_field_analyzer("moved", "renamed")
                .unwrap()
                .unwrap()
                .0,
            "changed"
        );
        engine.sql("DROP TABLE moved", &[]).unwrap();
        assert!(engine
            .durable
            .table_field_analyzers
            .read()
            .iter()
            .all(|(key, _)| key.0 != "public.moved"));
    }
}

#[test]
fn field_publication_rejects_missing_relations_before_catalog_or_image_membership() {
    let engine = Engine::new();
    engine.register_named_analyzer("saved", WORDS).unwrap();
    engine
        .sql(
            "CREATE TABLE docs(body text); CREATE INDEX body_index ON docs USING gin(body)",
            &[],
        )
        .unwrap();
    let original = engine.durable.table_field_analyzers.snapshot();
    assert!(engine
        .set_table_field_analyzer("missing", "body", "saved", "both")
        .unwrap_err()
        .contains("missing"));
    let current = engine.durable.table_field_analyzers.read();
    assert!(current
        .iter()
        .all(|(key, value)| std::ptr::eq(value, original.get(key).unwrap())));
    assert_eq!(current.len(), original.len());
}
