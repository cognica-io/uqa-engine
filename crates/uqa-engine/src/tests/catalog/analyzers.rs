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
