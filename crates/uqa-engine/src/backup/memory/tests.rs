//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod assembled;
use crate::Engine;
use uqa_storage::read_control::StorageReadControl;

#[test]
fn memory_catalog_units_keep_the_committed_generation_after_transaction_and_engine_retirement() {
    let engine = Engine::new();
    let setup = "CREATE ROLE reader; CREATE SCHEMA saved; CREATE TABLE saved.items(id integer); INSERT INTO saved.items VALUES(1); CREATE VIEW saved.visible AS SELECT id FROM saved.items; CREATE SEQUENCE saved.position; CREATE TYPE saved.described AS ENUM('original'); COMMENT ON TYPE saved.described IS 'original'; GRANT USAGE ON SCHEMA saved TO reader; GRANT SELECT ON saved.items TO reader";
    engine.sql(setup, &[]).unwrap();

    let source_control = StorageReadControl::with_limit(16 << 20);
    let cut = engine.capture_committed_memory(&source_control).unwrap();
    {
        let (name, table) = cut.next_table(None).unwrap().unwrap();
        assert_eq!(name.schema, "saved");
        assert_eq!(name.name, "items");
        assert_ne!(table.object_id, [0; 16]);
        assert!(cut.next_table(Some(name)).unwrap().is_none());
    }
    let mut expected = Vec::new();
    let output = StorageReadControl::with_limit(256 << 10);
    for slot in 1..=catalogs::CATALOG_COUNT {
        let mut reader = catalogs::image(&cut, slot, &output).unwrap().unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 47];
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        expected.push(bytes);
    }
    assert_eq!(output.memory().used(), 0);
    engine.sql("BEGIN; INSERT INTO saved.items VALUES(2); CREATE ROLE private_role; COMMENT ON TYPE saved.described IS 'private'", &[]).unwrap();
    let original = engine.capture_committed_memory(&source_control).unwrap();
    engine
        .sql("ROLLBACK; DROP SCHEMA saved CASCADE; DROP ROLE reader", &[])
        .unwrap();
    drop((cut, engine));
    std::thread::spawn(move || {
        for (slot, expected) in (1..=catalogs::CATALOG_COUNT).zip(expected) {
            let mut reader = catalogs::image(&original, slot, &output).unwrap().unwrap();
            let mut offset = 0;
            let mut buffer = [0; 17];
            loop {
                let count = reader.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                assert_eq!(
                    &buffer[..count],
                    &expected[offset..offset + count],
                    "catalog slot {slot}"
                );
                offset += count;
            }
            assert_eq!(offset, expected.len());
            assert_eq!(output.memory().used(), 0);
        }
        assert!(catalogs::image(&original, 25, &output).unwrap().is_none());
    })
    .join()
    .unwrap();
    assert_eq!(source_control.memory().used(), 0);
}

fn resource_bytes(source: &crate::state::SnapshotCut) -> Vec<(u8, String, Vec<u8>)> {
    let control = StorageReadControl::with_limit(256 << 10);
    let mut output = Vec::new();
    for kind in 1..=resources::RESOURCE_KINDS {
        let mut previous: Option<uqa_core::memory::Budgeted<String>> = None;
        loop {
            let next = resources::next(
                source,
                kind,
                previous.as_ref().map(|name| name.as_str()),
                &control,
            )
            .unwrap();
            let Some((name, mut reader)) = next else {
                break;
            };
            assert!(previous
                .as_ref()
                .is_none_or(|previous| previous.as_str() < name.as_str()));
            loop {
                let progress = reader.prepare(1).unwrap();
                assert!(progress.positions <= 1);
                if progress.ready {
                    break;
                }
            }
            let mut bytes = Vec::new();
            let mut buffer = [0; 31];
            loop {
                let count = reader.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            output.push((kind, (*name).clone(), bytes));
            previous = Some(name);
        }
    }
    assert_eq!(control.memory().used(), 0);
    output
}

#[test]
fn memory_resource_units_preserve_all_resource_kinds_after_replacement_and_engine_retirement() {
    use uqa_core::{Edge, Vertex};
    use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};
    let engine = Engine::new();
    for name in ["a", "z"] {
        for id in 1..=2 {
            engine
                .add_graph_vertex(Vertex::new(id, "node"), name)
                .unwrap();
        }
        engine
            .add_graph_edge(Edge::new(1, 1, 2, "edge"), name)
            .unwrap();
        engine
            .build_path_index("saved", name, &[vec!["edge".into()]])
            .unwrap();
        engine
            .save_model(
                name,
                &DeepModel {
                    layers: vec![DeepLayerSpec::Embed {
                        embedding: vec![0.25; 256],
                    }],
                    alpha: 0.25,
                    gating: GatingSpec::None,
                },
            )
            .unwrap();
        engine
            .register_named_analyzer(name, r#"{"tokenizer":{"type":"whitespace"}}"#)
            .unwrap();
    }
    let control = StorageReadControl::with_limit(4 << 20);
    let source = engine.capture_committed_memory(&control).unwrap();
    let expected = resource_bytes(&source);
    for kind in 1..=resources::RESOURCE_KINDS {
        assert_eq!(expected.iter().filter(|entry| entry.0 == kind).count(), 2);
    }
    for name in ["a", "z"] {
        engine.drop_graph(name).unwrap();
        engine.drop_model(name).unwrap();
        engine
            .register_named_analyzer(name, r#"{"tokenizer":{"type":"keyword"}}"#)
            .unwrap();
    }
    drop(engine);
    std::thread::spawn(move || assert_eq!(resource_bytes(&source), expected))
        .join()
        .unwrap();
    assert_eq!(control.memory().used(), 0);
}

fn table_bytes(table: &tables::TableOwner) -> Vec<Vec<u8>> {
    let output = StorageReadControl::with_limit(256 << 10);
    let mut images = Vec::new();
    for slot in 0..tables::TABLE_METADATA_COUNT {
        let mut reader = tables::image(table, slot, &output).unwrap().unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 19];
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        images.push(bytes);
    }
    assert_eq!(output.memory().used(), 0);
    images
}

#[test]
fn memory_table_units_preserve_native_properties_and_bound_metadata_after_retirement() {
    let engine = Engine::new();
    engine.sql("CREATE UNLOGGED TABLE saved(id integer PRIMARY KEY, body text DEFAULT 'original' CHECK(length(body)>0)); CREATE INDEX saved_text ON saved USING gin(body); INSERT INTO saved VALUES(1,'original'); ANALYZE saved", &[]).unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let cut = engine.capture_committed_memory(&control).unwrap();
    let (_, value) = cut.next_table(None).unwrap().unwrap();
    let table = tables::retain(value, &control).unwrap();
    let expected = table_bytes(&table);
    let input = StorageReadControl::with_limit(16 << 10);
    let properties =
        uqa_storage::backup::read_memory_table_metadata(&mut expected[0].as_slice(), &input)
            .unwrap();
    assert_eq!(properties.properties.object_id, table.object_id);
    assert_eq!(properties.properties.persistence, 1);
    assert_eq!(properties.properties.next_id, *table.next_id.get().unwrap());
    assert_eq!(
        properties.properties.columns_declared,
        *table.columns_declared.get().unwrap()
    );
    assert_eq!(
        properties.properties.statistics.loaded,
        *table.column_stats_loaded.get().unwrap()
    );
    assert_eq!(
        properties.analyzer.as_slice(),
        table.analyzer.get().unwrap().as_bytes()
    );
    let (properties, parent_memory) = properties.into_parts();
    let fields = uqa_sql::catalog::table_metadata::TableMetadata::from_admitted(
        properties.fts_fields,
        input.cancellation(),
    )
    .unwrap();
    let array_name = uqa_sql::catalog::table_metadata::TableMetadata::from_admitted(
        properties.row_type_array_name,
        input.cancellation(),
    )
    .unwrap();
    assert_eq!(&*fields, &**table.fts_fields.get().unwrap());
    assert_eq!(&*array_name, &**table.row_type_array_name.get().unwrap());
    let analyzer =
        uqa_analysis::AnalyzerDefinition::from_image(properties.analyzer, input.cancellation())
            .unwrap();
    drop(parent_memory);
    assert!(input.memory().used() > 0);
    engine.sql("BEGIN; UPDATE saved SET body='private'; ROLLBACK; DROP TABLE saved; CREATE TABLE saved(replacement integer)", &[]).unwrap();
    drop((cut, engine));
    std::thread::spawn(move || {
        assert_eq!(table_bytes(&table), expected);
        assert_eq!(&*fields, &**table.fts_fields.get().unwrap());
        assert_eq!(&*array_name, &**table.row_type_array_name.get().unwrap());
        assert_eq!(
            serde_json::to_vec(&analyzer).unwrap(),
            table.analyzer.get().unwrap().as_bytes()
        );
        let image = analyzer
            .capture_image(&uqa_core::memory::ProductionControl::uncontrolled())
            .unwrap();
        assert_eq!(image.as_bytes(), table.analyzer.get().unwrap().as_bytes());
    })
    .join()
    .unwrap();
    assert_eq!(control.memory().used(), 0);
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn memory_catalog_and_table_readers_keep_original_cancellation_after_opening() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE saved(id integer)", &[]).unwrap();
    for table_image in [false, true] {
        let original = StorageReadControl::with_limit(8 << 20);
        let cut = engine.capture_committed_memory(&original).unwrap();
        let invoking = StorageReadControl::with_limit(256 << 10);
        let mut reader = if table_image {
            let (_, table) = cut.next_table(None).unwrap().unwrap();
            tables::image(&tables::retain(table, &original).unwrap(), 0, &invoking)
                .unwrap()
                .unwrap()
        } else {
            catalogs::image(&cut, 9, &invoking).unwrap().unwrap()
        };
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
        original.cancellation().cancel();
        assert!(reader.read(&mut [0; 1]).is_err());
        assert_eq!(invoking.memory().used(), 0);
        assert!(reader.read(&mut [0; 1]).is_err());
        drop((cut, reader));
        assert_eq!(original.memory().used(), 0);
    }
}

fn field_readers(
    cut: &crate::state::SnapshotCut,
    control: &StorageReadControl,
) -> Vec<(
    uqa_core::memory::Budgeted<field_analyzers::Key>,
    uqa_storage::image_io::ImageReader<'static>,
)> {
    let mut result: Vec<(
        uqa_core::memory::Budgeted<field_analyzers::Key>,
        uqa_storage::image_io::ImageReader<'static>,
    )> = Vec::new();
    loop {
        let after = result.last().map(|(key, _)| &**key);
        let Some(next) = field_analyzers::next(cut, after, control).unwrap() else {
            return result;
        };
        assert!(after.is_none_or(|after| after < &*next.0));
        result.push(next);
    }
}

fn engine_with_private_field_replacement() -> Engine {
    let engine = Engine::new();
    engine.sql("CREATE TABLE a(body text); CREATE INDEX a_body ON a USING gin(body); CREATE TABLE z(body text); CREATE INDEX z_body ON z USING gin(body); CREATE TEMP TABLE temporary(body text); CREATE INDEX temp_body ON temporary USING gin(body)", &[]).unwrap();
    engine
        .register_named_analyzer("saved", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    for table in ["a", "z"] {
        engine
            .set_table_field_analyzer(table, "body", "saved", "index")
            .unwrap();
    }
    engine
        .register_named_analyzer("saved", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    for table in ["a", "z"] {
        engine
            .set_table_field_analyzer(table, "body", "saved", "search")
            .unwrap();
    }
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .register_named_analyzer("saved", r#"{"tokenizer":{"type":"standard"}}"#)
        .unwrap();
    engine
        .set_table_field_analyzer("a", "body", "saved", "both")
        .unwrap();
    engine
}

#[test]
fn memory_field_images_keep_committed_historical_sides_after_cut_and_engine_retirement() {
    use uqa_storage::analyzer_binding::FieldAnalyzerBinding;
    let engine = engine_with_private_field_replacement();
    let original = StorageReadControl::with_limit(8 << 20);
    let cut = engine.capture_committed_memory(&original).unwrap();
    let output = StorageReadControl::with_limit(256 << 10);
    let readers = field_readers(&cut, &output);
    assert_eq!(readers.len(), 2);
    engine
        .sql(
            "ROLLBACK; DROP TABLE a; DROP TABLE z; DROP TABLE temporary",
            &[],
        )
        .unwrap();
    drop((cut, engine));
    std::thread::spawn(move || {
        let resources =
            uqa_analysis::AnalyzerResources::new(uqa_analysis::AnalyzerLimits::default());
        for ((key, mut reader), table) in readers.into_iter().zip(["a", "z"]) {
            assert_eq!(
                key.0,
                uqa_core::RelationIdentity::new("public", table).qualified_name()
            );
            assert_eq!(key.1, "body");
            let mut bytes = Vec::new();
            let mut buffer = [0; 31];
            loop {
                let count = reader.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            let input = StorageReadControl::with_limit(1 << 20);
            let binding =
                FieldAnalyzerBinding::read_image(&mut bytes.as_slice(), &resources, &input)
                    .unwrap();
            assert_eq!(binding.index.name.as_deref(), Some("saved"));
            assert_eq!(binding.search.name.as_deref(), Some("saved"));
            assert_eq!(binding.last_phase, uqa_storage::AnalyzerPhase::Search);
            assert_eq!(binding.owner, uqa_storage::AnalyzerBindingOwner::Field);
            assert_eq!(
                binding
                    .index
                    .compiled
                    .analyze_tokens("one two")
                    .unwrap()
                    .tokens()
                    .len(),
                2
            );
            assert_eq!(
                binding
                    .search
                    .compiled
                    .analyze_tokens("one two")
                    .unwrap()
                    .tokens()
                    .len(),
                1
            );
            drop(binding);
            assert_eq!(input.memory().used(), 0);
        }
        assert_eq!(output.memory().used(), 0);
    })
    .join()
    .unwrap();
    assert_eq!(original.memory().used(), 0);
}
