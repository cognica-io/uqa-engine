//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn document(id: i64, value: i64) -> Document {
    BTreeMap::from([
        ("id".into(), Value::Int(id)),
        ("value".into(), Value::Int(value)),
    ])
}

#[test]
fn command_overlay_scan_merges_persisted_and_staged_rows_in_document_order() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE command_scan (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO command_scan VALUES (1, 10), (2, 20), (4, 40)",
            &[],
        )
        .unwrap();
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine
        .stage_command_document("command_scan", 2, Some(document(2, 200)))
        .unwrap();
    engine
        .stage_command_document("command_scan", 3, Some(document(3, 300)))
        .unwrap();
    engine
        .stage_command_document("command_scan", 4, None)
        .unwrap();
    engine
        .stage_command_document("command_scan", 5, Some(document(5, 500)))
        .unwrap();

    let result = engine
        .sql("SELECT id, value FROM command_scan ORDER BY id", &[])
        .unwrap();

    assert_eq!(engine.table_doc_count("command_scan").unwrap(), 4);
    assert_eq!(engine.table_doc_ids("command_scan").unwrap(), [1, 2, 3, 5]);
    assert_eq!(result.rows.len(), 4);
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(10)));
    assert_eq!(result.value_at(1, 1), Some(&Value::Int(200)));
    assert_eq!(result.value_at(2, 1), Some(&Value::Int(300)));
    assert_eq!(result.value_at(3, 1), Some(&Value::Int(500)));
    engine.mutation_coordinator().end_command_mutation_overlay();
}

#[test]
fn command_overlay_scan_pages_without_losing_filtered_or_changed_rows() {
    use std::sync::atomic::Ordering;

    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE paged_command_scan (id INTEGER PRIMARY KEY, value INTEGER)",
            &[],
        )
        .unwrap();
    let table = engine.require_table("paged_command_scan").unwrap();
    {
        let mut store = table.document_store.write();
        for doc_id in 1..=2050 {
            let value = i64::try_from(doc_id).unwrap();
            store.put(doc_id, document(value, value)).unwrap();
        }
    }
    table.doc_count_dirty.store(true, Ordering::Release);
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine
        .stage_command_document("paged_command_scan", 2, Some(document(2, 9002)))
        .unwrap();
    engine
        .stage_command_document("paged_command_scan", 1025, Some(document(1025, 9025)))
        .unwrap();
    engine
        .stage_command_document("paged_command_scan", 2048, None)
        .unwrap();
    engine
        .stage_command_document("paged_command_scan", 4096, Some(document(4096, 9999)))
        .unwrap();

    let ids = engine
        .sql(
            "SELECT id FROM paged_command_scan WHERE value >= 2047 ORDER BY id",
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["id"].clone())
        .collect::<Vec<_>>();

    assert_eq!(
        ids,
        [2, 1025, 2047, 2049, 2050, 4096].map(Value::Int).to_vec()
    );
    engine.mutation_coordinator().end_command_mutation_overlay();
}

#[test]
fn overlay_projection_materializes_only_requested_virtual_columns() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE overlay_virtual (id INTEGER PRIMARY KEY, source INTEGER, derived INTEGER GENERATED ALWAYS AS (1 / source) VIRTUAL); INSERT INTO overlay_virtual (id, source) VALUES (1, 1), (2, 2)",
            &[],
        )
        .unwrap();
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine
        .stage_command_document(
            "overlay_virtual",
            1,
            Some(BTreeMap::from([
                ("id".into(), Value::Int(1)),
                ("source".into(), Value::Int(0)),
            ])),
        )
        .unwrap();
    engine
        .stage_command_document("overlay_virtual", 2, None)
        .unwrap();
    engine
        .stage_command_document(
            "overlay_virtual",
            3,
            Some(BTreeMap::from([
                ("id".into(), Value::Int(3)),
                ("source".into(), Value::Int(3)),
            ])),
        )
        .unwrap();

    let documents = engine
        .get_documents_with_materialized_projection(
            "overlay_virtual",
            &[1, 2, 3],
            &["source".into()],
        )
        .unwrap();
    assert_eq!(documents[&1]["source"], Value::Int(0));
    assert!(!documents.contains_key(&2));
    assert_eq!(documents[&3]["source"], Value::Int(3));
    let fields = engine
        .get_query_document_fields_multi("overlay_virtual", &[1, 2, 3], &["source"])
        .unwrap();
    assert_eq!(fields[&1], [Value::Int(0)]);
    assert!(!fields.contains_key(&2));
    assert_eq!(fields[&3], [Value::Int(3)]);
    assert!(engine
        .get_documents_with_materialized_projection("overlay_virtual", &[1], &["derived".into()],)
        .is_err());
    engine.mutation_coordinator().end_command_mutation_overlay();
}
