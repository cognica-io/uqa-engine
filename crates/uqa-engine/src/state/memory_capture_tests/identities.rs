//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};

fn fixture(engine: &Engine) {
    sql(engine, "CREATE SEQUENCE saved_sequence START WITH 41 INCREMENT BY 3 CACHE 4; SELECT nextval('saved_sequence'); CREATE UNLOGGED SEQUENCE unlogged_sequence; CREATE TYPE described_type AS ENUM('original'); COMMENT ON TYPE described_type IS 'original description'; CREATE TABLE indexed_table(id integer, body text); CREATE INDEX saved_index ON indexed_table(id)");
    engine.create_graph("saved_graph").unwrap();
    engine
        .add_graph_vertex(uqa_core::Vertex::new(1, "item"), "saved_graph")
        .unwrap();
    engine
        .save_scoring_params("signal", "{ \"alpha\": 1.00 }")
        .unwrap();
}

#[test]
fn memory_metadata_capture_preserves_identity_position_and_encoded_values_across_lifecycle() {
    for mutate in [false, true] {
        let engine = Engine::new();
        fixture(&engine);
        let sequences = engine.durable.sequences.snapshot();
        let objects = engine.durable.sequence_object_ids.snapshot();
        let oids = engine.durable.sequence_catalog_oids.snapshot();
        let persistence = engine.durable.sequence_persistence.snapshot();
        let graph_oids = engine.durable.graph_catalog_oids.snapshot();
        let comments = engine.durable.comments.snapshot();
        let indexes = engine.durable.catalog_indexes.snapshot();
        let params = engine.durable.scoring_params.snapshot();
        assert!(
            !sequences.is_empty()
                && !objects.is_empty()
                && !oids.is_empty()
                && !persistence.is_empty()
        );
        assert!(
            !graph_oids.is_empty()
                && !comments.is_empty()
                && !indexes.is_empty()
                && !params.is_empty()
        );
        let sequence_key = engine.durable.sequences.key().unwrap();
        let object_key = engine.durable.sequence_object_ids.key().unwrap();
        let oid_key = engine.durable.sequence_catalog_oids.key().unwrap();
        let persistence_key = engine.durable.sequence_persistence.key().unwrap();
        let graph_key = engine.durable.graph_catalog_oids.key().unwrap();
        let comment_key = engine.durable.comments.key().unwrap();
        let index_key = engine.durable.catalog_indexes.key().unwrap();
        let params_key = engine.durable.scoring_params.key().unwrap();
        let control = StorageReadControl::with_limit(4 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine, "BEGIN; ALTER SEQUENCE saved_sequence RESTART WITH 99; COMMENT ON TYPE described_type IS 'private'; SAVEPOINT changed; DROP INDEX saved_index; ROLLBACK TO changed; ROLLBACK");
            sql(&engine, "DROP SEQUENCE saved_sequence; CREATE SEQUENCE saved_sequence START 1; DROP TYPE described_type; CREATE TYPE described_type AS ENUM('replacement'); DROP INDEX saved_index; CREATE INDEX saved_index ON indexed_table(body)");
            engine.drop_graph("saved_graph").unwrap();
            engine.create_graph("saved_graph").unwrap();
            engine
                .save_scoring_params("signal", "{\"alpha\":2}")
                .unwrap();
        }
        drop(engine);
        macro_rules! same {
            ($key:expr, $source:expr) => {{
                let saved = $key.retained(&cut).unwrap().unwrap();
                assert!(saved
                    .get()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k, v))
                    .eq($source.iter()));
                saved
            }};
        }
        let saved_sequences = same!(sequence_key, sequences);
        let saved_objects = same!(object_key, objects);
        let saved_oids = same!(oid_key, oids);
        let saved_persistence = same!(persistence_key, persistence);
        let saved_comments = same!(comment_key, comments);
        let saved_params = params_key.retained(&cut).unwrap().unwrap();
        assert!(saved_params.get().unwrap().iter().eq(params.iter()));
        let saved_indexes = index_key.retained(&cut).unwrap().unwrap();
        same_indexes(saved_indexes.get().unwrap(), &indexes);
        let saved_graph = graph_key.retained(&cut).unwrap().unwrap();
        same_graphs(saved_graph.get().unwrap(), &graph_oids);
        drop(cut);
        control.cancellation().cancel();
        macro_rules! cancelled { ($($value:expr),+ $(,)?) => { $(assert!($value.get().is_err());)+ }; }
        cancelled!(
            saved_sequences,
            saved_objects,
            saved_oids,
            saved_persistence,
            saved_comments,
            saved_params,
            saved_indexes,
            saved_graph
        );
        drop((
            saved_sequences,
            saved_objects,
            saved_oids,
            saved_persistence,
            saved_comments,
            saved_params,
            saved_indexes,
            saved_graph,
        ));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn denied_metadata_capture_does_not_prevent_scoring_parameter_publication() {
    let engine = Engine::new();
    let original = format!("{{\"payload\":\"{}\"}}", "x".repeat(32768));
    engine.save_scoring_params("signal", &original).unwrap();
    let key = engine.durable.scoring_params.key().unwrap();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let denied = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    engine.save_scoring_params("signal", "{}").unwrap();
    assert_eq!(
        engine.load_scoring_params("signal").unwrap().as_deref(),
        Some("{}")
    );
    assert!(denied.check().is_err());
    let saved = key.retained(&cut).unwrap().unwrap();
    assert_eq!(saved.get().unwrap().len(), 1);
    assert_eq!(saved.get().unwrap().get("signal"), Some(&original));
    drop((engine, saved, cut, denied));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}

#[test]
fn committed_scoring_capture_and_later_writes_do_not_copy_unrelated_payloads() {
    let engine = Engine::new();
    let original = format!("{{\"payload\":\"{}\"}}", "x".repeat(4 << 20));
    engine.save_scoring_params("large", &original).unwrap();
    engine
        .save_scoring_params("small", "{ \"x\": 1.00 }")
        .unwrap();
    let pointer = engine
        .durable
        .scoring_params
        .read()
        .get("large")
        .unwrap()
        .as_ptr();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut cut = None;
    let measured = allocation_counter::measure(|| {
        cut = Some(engine.capture_committed_memory(&control).unwrap());
    });
    assert!(measured.bytes_total < 1 << 19, "{measured:?}");
    let cut = cut.unwrap();
    let saved = cut.resources.scoring_params.captured(&cut.cut).unwrap();
    assert_eq!(saved.get().unwrap().get("large").unwrap().as_ptr(), pointer);
    let retained = control.memory().used();
    let measured =
        allocation_counter::measure(|| engine.save_scoring_params("small", "{}").unwrap());
    assert!(measured.bytes_total < 1 << 19, "{measured:?}");
    sql(&engine, "BEGIN");
    engine.save_scoring_params("private", "{\"x\":2}").unwrap();
    sql(&engine, "SAVEPOINT saved_parameters");
    assert!(engine.drop_scoring_params("large").unwrap());
    sql(&engine, "ROLLBACK TO saved_parameters; ROLLBACK");
    assert_eq!(
        engine.load_scoring_params("large").unwrap().as_deref(),
        Some(original.as_str())
    );
    assert!(engine.load_scoring_params("private").unwrap().is_none());
    engine
        .save_scoring_params("later", &"y".repeat(8 << 20))
        .unwrap();
    drop(engine);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(saved.get().unwrap().len(), 2);
    assert_eq!(saved.get().unwrap().get("large"), Some(&original));
    assert_eq!(
        saved.get().unwrap().get("small").map(String::as_str),
        Some("{ \"x\": 1.00 }")
    );
    control.cancellation().cancel();
    assert!(saved.get().is_err());
    drop((saved, cut));
    assert_eq!(control.memory().used(), 0);
}

fn same_indexes(
    saved: &[(uqa_core::RelationIdentity, uqa_storage::CatalogIndexRow)],
    original: &std::collections::BTreeMap<uqa_core::RelationIdentity, uqa_storage::CatalogIndexRow>,
) {
    assert_eq!(saved.len(), original.len());
    for ((key, after), (name, before)) in saved.iter().zip(original.iter()) {
        assert_eq!(key, name);
        assert_eq!(after.relation, before.relation);
        assert_eq!(
            (
                &after.index_type,
                &after.table_name,
                &after.columns_json,
                &after.parameters_json,
                &after.definition_json
            ),
            (
                &before.index_type,
                &before.table_name,
                &before.columns_json,
                &before.parameters_json,
                &before.definition_json
            )
        );
    }
}

fn same_graphs(
    saved: &[(
        String,
        uqa_sql::catalog::definition_capture::identities::CapturedGraphCatalogOids,
    )],
    original: &std::collections::BTreeMap<String, uqa_sql::catalog::graph_oids::GraphCatalogOids>,
) {
    assert_eq!(saved.len(), original.len());
    for ((key, after), (name, before)) in saved.iter().zip(original.iter()) {
        assert_eq!(key, name);
        assert_eq!(
            (after.namespace, after.label_sequence),
            (before.namespace, before.label_sequence)
        );
        assert_eq!(after.labels.len(), before.labels.len());
        for ((id, after), (original_id, before)) in after.labels.iter().zip(before.labels.iter()) {
            assert_eq!(id, original_id);
            assert_eq!(
                (
                    after.sequence,
                    after.relation,
                    after.id_default,
                    after.properties_default,
                    after.toast_table,
                    after.toast_index,
                    after.primary_key,
                    after.endpoint_indexes,
                    after.trigger
                ),
                (
                    before.sequence,
                    before.relation,
                    before.id_default,
                    before.properties_default,
                    before.toast_table,
                    before.toast_index,
                    before.primary_key,
                    before.endpoint_indexes,
                    before.trigger
                )
            );
            assert!(after
                .not_null
                .iter()
                .map(|(k, v)| (k, v))
                .eq(before.not_null.iter()));
        }
    }
}
