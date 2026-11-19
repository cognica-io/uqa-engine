//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::{AnalyzerDefinition, AnalyzerResources};
use uqa_core::memory::{Budgeted, BudgetedString};
use uqa_storage::{inverted_index::AnalyzerDefault, read_control::StorageReadControl, StorageName};

fn install(engine: &Engine) -> (Arc<crate::TableState>, StorageReadControl) {
    engine.sql("CREATE TABLE notes(id integer, body text); CREATE INDEX notes_body ON notes USING gin(body); INSERT INTO notes VALUES(1, 'original')", &[]).unwrap();
    let table = engine.require_table("notes").unwrap();
    let control = install_handles(engine, &table);
    (table, control)
}

fn install_handles(engine: &Engine, table: &crate::TableState) -> StorageReadControl {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut name = BudgetedString::new(control.memory());
    name.push_str("public.notes").unwrap();
    let (name, memory) = name.into_parts();
    let name = StorageName::from_admitted(Budgeted::new(name, memory)).unwrap();
    let bytes = serde_json::to_vec(table.analyzer.read().configuration()).unwrap();
    let memory = control.memory().reserve(bytes.capacity()).unwrap();
    let definition =
        AnalyzerDefinition::from_image(Budgeted::new(bytes, memory), control.cancellation())
            .unwrap();
    let analyzer =
        AnalyzerDefault::from_definition(definition, AnalyzerResources::default(), &control)
            .unwrap();
    let backend = engine.storage.backend.as_ref().unwrap();
    *table.document_store.write() = backend
        .document_store_with_control(name.clone(), &control)
        .unwrap();
    *table.inverted_index.write() = backend
        .inverted_index_with_control(name, analyzer, &control)
        .unwrap();
    assert!(control.memory().used() > 0);
    control.cancellation().cancel();
    control
}

#[test]
fn persistent_table_handles_keep_allowance_until_storage_replacement() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent(provider, &directory.path().join("replacement.db"));
        let (table, control) = install(&engine);
        engine
            .rebind_persistent_table_stores("public.notes", &table)
            .unwrap();
        assert_eq!(control.memory().used(), 0, "provider {provider}");
        let control = install_handles(&engine, &table);
        engine
            .sql("INSERT INTO notes VALUES(2, 'retained')", &[])
            .unwrap();
        assert_eq!(
            engine
                .sql("SELECT body FROM notes ORDER BY id", &[])
                .unwrap()
                .rows[1]["body"],
            Value::Str("retained".into())
        );
        // The standard analyzer stores Porter stems.
        assert_eq!(
            table
                .inverted_index
                .read()
                .doc_freq("body", "retain")
                .unwrap(),
            1
        );
        // Ordinary legacy session refresh can replace physical handles after its own writes.
        engine
            .rebind_persistent_table_stores("public.notes", &table)
            .unwrap();
        assert_eq!(control.memory().used(), 0, "provider {provider}");
        assert!(Arc::ptr_eq(&table, &engine.require_table("notes").unwrap()));
        engine
            .sql("BEGIN; UPDATE notes SET body='discarded'; ROLLBACK", &[])
            .unwrap();
        assert_eq!(
            engine
                .sql("SELECT body FROM notes ORDER BY id", &[])
                .unwrap()
                .rows[1]["body"],
            Value::Str("retained".into())
        );
        assert_eq!(
            table
                .inverted_index
                .read()
                .doc_freq("body", "discard")
                .unwrap(),
            0
        );
    }
}

#[test]
fn retained_tables_keep_storage_allowance_after_the_engine_closes() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent(provider, &directory.path().join("retirement.db"));
        let (table, control) = install(&engine);
        let bytes = control.memory().used();
        drop(engine);
        assert_eq!(control.memory().used(), bytes, "provider {provider}");
        assert_eq!(table.document_store.read().len().unwrap(), 1);
        assert_eq!(
            table
                .inverted_index
                .read()
                .doc_freq("body", "origin")
                .unwrap(),
            1
        );
        drop(table);
        assert_eq!(control.memory().used(), 0, "provider {provider}");
    }
}
