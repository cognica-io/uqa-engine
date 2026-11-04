//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_analysis::CompiledAnalyzer;
use uqa_storage::{
    analyzer_image::{write_analyzer_image, AnalyzerImageSnapshot},
    StorageBackendError,
};

const WORDS: &str = r#"{"tokenizer":{"type":"whitespace"}}"#;
const WHOLE: &str = r#"{"tokenizer":{"type":"keyword"}}"#;
const STANDARD: &str = r#"{"tokenizer":{"type":"standard"}}"#;

fn image(value: &CompiledAnalyzer) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_analyzer_image(value, &mut bytes, &StorageReadControl::with_limit(0)).unwrap();
    bytes
}

fn saved_image(value: &AnalyzerImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    value
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

fn original_images(engine: &Engine) -> [Vec<u8>; 3] {
    let names = engine.durable.named_analyzers.read();
    let bindings = engine.durable.table_field_analyzers.read();
    let binding = bindings.values().next().unwrap();
    [
        image(&names["saved"]),
        image(&binding.index.compiled),
        image(&binding.search.compiled),
    ]
}

#[test]
fn memory_analyzer_capture_keeps_named_and_historical_field_revisions_through_rollback_and_closure()
{
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE TABLE docs(id integer, body text); CREATE INDEX body_idx ON docs USING gin(body)");
        engine.register_named_analyzer("saved", WORDS).unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "saved", "index")
            .unwrap();
        engine.register_named_analyzer("saved", WHOLE).unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "saved", "search")
            .unwrap();
        let expected = original_images(&engine);
        assert_ne!(expected[1], expected[2]);
        assert_eq!(expected[0], expected[2]);
        let names = engine.durable.named_analyzers.key().unwrap();
        let fields = engine.durable.table_field_analyzers.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine, "BEGIN");
            engine.register_named_analyzer("saved", STANDARD).unwrap();
            engine
                .set_table_field_analyzer("docs", "body", "saved", "both")
                .unwrap();
            sql(
                &engine,
                "SAVEPOINT changed; DROP TABLE docs; ROLLBACK TO changed; ROLLBACK",
            );
            assert_eq!(original_images(&engine), expected);
            sql(&engine, "DROP TABLE docs");
            assert!(engine.drop_named_analyzer("saved").unwrap());
            engine.register_named_analyzer("saved", STANDARD).unwrap();
            sql(
                &engine,
                "CREATE TABLE docs(body text); CREATE INDEX body_idx ON docs USING gin(body)",
            );
            engine
                .set_table_field_analyzer("docs", "body", "saved", "both")
                .unwrap();
            assert_ne!(original_images(&engine), expected);
        }
        drop(engine);
        let names = names.retained(&cut).unwrap().unwrap();
        let fields = fields.retained(&cut).unwrap().unwrap();
        assert_eq!(names.get().unwrap().len(), 1);
        assert_eq!(fields.get().unwrap().len(), 1);
        let binding = &fields.get().unwrap()[0].1;
        assert_eq!(binding.index.name.as_deref(), Some("saved"));
        assert_eq!(binding.search.name.as_deref(), Some("saved"));
        assert_eq!(binding.owner, uqa_storage::AnalyzerBindingOwner::Field);
        assert_eq!(binding.last_phase, uqa_storage::AnalyzerPhase::Search);
        let children = [
            names.get().unwrap().image("saved").unwrap().unwrap(),
            binding.index.image.clone(),
            binding.search.image.clone(),
        ];
        drop((cut, names, fields));
        for (child, expected) in children.iter().zip(expected) {
            assert_eq!(saved_image(child), expected);
        }
        control.cancellation().cancel();
        for child in &children {
            let mut output = Vec::new();
            assert!(matches!(
                child.write_image(&mut output, &StorageReadControl::with_limit(0)),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert!(output.is_empty());
        }
        drop(children);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn denied_analyzer_capture_preserves_replacement_and_independently_funded_images() {
    let engine = Engine::new();
    let name = "analyzer".repeat(8192);
    engine.register_named_analyzer(&name, WORDS).unwrap();
    let expected = image(&engine.durable.named_analyzers.read()[&name]);
    let key = engine.durable.named_analyzers.key().unwrap();
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let small = StorageReadControl::with_limit(8192);
    let good = StorageReadControl::with_limit(1 << 20);
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    engine.register_named_analyzer(&name, WHOLE).unwrap();
    assert!(matches!(
        failed.check(),
        Err(StorageBackendError::Memory(_))
    ));
    assert_ne!(
        image(&engine.durable.named_analyzers.read()[&name]),
        expected
    );
    drop(engine);
    let saved = key.retained(&cut).unwrap().unwrap();
    assert_eq!(
        saved_image(&saved.get().unwrap().image(&name).unwrap().unwrap()),
        expected
    );
    drop((saved, cut, failed));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
