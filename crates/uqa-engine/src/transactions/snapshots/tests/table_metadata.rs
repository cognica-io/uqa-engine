//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use uqa_storage::analyzer_image::{write_analyzer_image, AnalyzerImageSnapshot};

const RELATION: &str = "\"Saved.Schema\".\"Docs\"";

fn bytes(image: &AnalyzerImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_table_metadata_preserves_original_relations_and_historical_analyzers(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    sql(&engine, "CREATE SCHEMA \"Saved.Schema\"; CREATE TABLE \"Saved.Schema\".\"Docs\"(id integer, body text); INSERT INTO \"Saved.Schema\".\"Docs\" VALUES(1,'original text'); CREATE INDEX body_index ON \"Saved.Schema\".\"Docs\" USING gin(body); CREATE INDEX id_index ON \"Saved.Schema\".\"Docs\"(id); CREATE UNLOGGED TABLE unlogged_docs(id integer); CREATE INDEX unlogged_index ON unlogged_docs(id); CREATE TEMP TABLE temporary_docs(body text); CREATE INDEX temporary_index ON temporary_docs USING gin(body)");
    engine
        .register_named_analyzer("saved", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    engine
        .set_table_field_analyzer(RELATION, "body", "saved", "index")
        .unwrap();
    engine
        .register_named_analyzer("saved", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    engine
        .set_table_field_analyzer(RELATION, "body", "saved", "search")
        .unwrap();
    engine
        .set_table_field_analyzer("temporary_docs", "body", "saved", "both")
        .unwrap();
    let expected = {
        let bindings = engine.durable.table_field_analyzers.read();
        let binding = &bindings[&(RELATION.into(), "body".into())];
        [&binding.index, &binding.search].map(|side| {
            let mut bytes = Vec::new();
            write_analyzer_image(
                &side.compiled,
                &mut bytes,
                &StorageReadControl::with_limit(0),
            )
            .unwrap();
            bytes
        })
    };
    assert_ne!(expected[0], expected[1]);
    let indexes = engine.durable.catalog_indexes.snapshot();
    let original = [
        RelationIdentity::new("Saved.Schema", "body_index"),
        RelationIdentity::new("Saved.Schema", "id_index"),
        RelationIdentity::new("public", "unlogged_index"),
    ]
    .map(|name| indexes[&name].clone());
    drop(indexes);
    let temporary = engine
        .durable
        .catalog_indexes
        .read()
        .values()
        .find(|row| row.relation.name == "temporary_index")
        .unwrap()
        .table_name
        .clone();
    let original_indexes = engine.durable.catalog_indexes.snapshot();
    let mut next_indexes = original_indexes.as_ref().clone();
    for mut row in original_indexes.values().cloned() {
        if row.table_name == temporary {
            row.definition_json
                .as_mut()
                .unwrap()
                .push_str(&" ".repeat(1 << 20));
            next_indexes.replace(row).unwrap();
        }
    }
    *engine.durable.catalog_indexes.write() = next_indexes;
    drop(original_indexes);
    {
        let mut fields = engine.durable.table_field_analyzers.write();
        let key = (temporary, "body".into());
        let mut binding = fields.get(&key).unwrap().clone();
        binding.index.name = Some("x".repeat(1 << 20));
        fields
            .insert(&key, binding, true, &uqa_core::CancellationToken::new())
            .unwrap();
    }
    let weak_indexes = engine.durable.catalog_indexes.snapshot().identity();
    let weak_analyzers = engine.durable.table_field_analyzers.snapshot().identity();
    sql(&engine, "BEGIN; UPDATE \"Saved.Schema\".\"Docs\" SET body='private'; DROP INDEX \"Saved.Schema\".id_index; CREATE INDEX id_index ON \"Saved.Schema\".\"Docs\"(body)");
    engine
        .register_named_analyzer("saved", r#"{"tokenizer":{"type":"standard"}}"#)
        .unwrap();
    engine
        .set_table_field_analyzer(RELATION, "body", "saved", "both")
        .unwrap();
    sql(
        &engine,
        "SAVEPOINT nested; DROP TABLE unlogged_docs; ROLLBACK TO nested",
    );
    let control = StorageReadControl::with_limit(128 << 10);
    let selected = capture(&engine, &control);
    assert!(selected.metadata.retained(&selected.cut).unwrap().is_none());
    sql(&engine, completion);
    sql(&engine, "DROP TABLE \"Saved.Schema\".\"Docs\"; CREATE TABLE \"Saved.Schema\".\"Docs\"(replacement boolean)");
    drop(engine);
    assert!(!weak_indexes.is_alive());
    assert!(!weak_analyzers.is_alive());
    let saved = selected.metadata.captured(&selected.cut).unwrap();
    let metadata = saved.get().unwrap();
    assert_eq!(metadata.indexes.len(), original.len());
    for expected in original {
        let (name, row) = metadata
            .indexes
            .iter()
            .find(|(name, _)| **name == expected.relation)
            .unwrap();
        assert_eq!(*name, row.relation);
        assert_eq!(row.relation, expected.relation);
        assert_eq!(row.table_name, expected.table_name);
        assert_eq!(row.index_type, expected.index_type);
        assert_eq!(row.columns_json, expected.columns_json);
        assert_eq!(row.parameters_json, expected.parameters_json);
        assert_eq!(row.definition_json, expected.definition_json);
    }
    assert_eq!(metadata.analyzers.len(), 1);
    let name = metadata.analyzers.keys().next().unwrap();
    let binding = metadata.analyzers.binding(name).unwrap().unwrap();
    assert_eq!(name, &(RELATION.into(), "body".into()));
    assert_eq!(
        [bytes(&binding.index.image), bytes(&binding.search.image)],
        expected
    );
    let table = selected
        .table(&RelationIdentity::new("Saved.Schema", "Docs"))
        .unwrap();
    assert_eq!(
        table.document_store.get_field(1, "body").unwrap(),
        Some(Value::Str("original text".into()))
    );
    drop((table, selected));
    control.cancellation().cancel();
    assert!(binding
        .index
        .image
        .write_image(&mut Vec::new(), &StorageReadControl::with_limit(0))
        .is_err());
    assert!(saved.get().is_err());
    drop((binding, saved));
    assert_eq!(control.memory().used(), 0);
}
