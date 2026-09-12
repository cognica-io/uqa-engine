//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_core::Value;
use uqa_execution::RowSchemaExecution;
use uqa_execution::RowSource;
use uqa_execution::{query::scored_input::*, row_locks::recheck::RecheckDoc};
use uqa_sql::expr::RowLookup;
use uqa_sql::ResultRow;

#[test]
fn pruned_primary_key_is_not_advertised_as_output_ordering() {
    let engine = crate::Engine::new();
    engine
        .sql(
            "CREATE TABLE ordered_source (id BIGINT PRIMARY KEY, payload TEXT)",
            &[],
        )
        .unwrap();
    let table = engine.require_table("ordered_source").unwrap();

    let pruned = ScoredDocumentSource::new(
        "ordered_source",
        table.clone(),
        ScoredInput::All,
        vec!["payload".into()],
        Some("id".into()),
        None,
    );
    assert!(pruned.output_ordering().is_empty());

    let retained = ScoredDocumentSource::new(
        "ordered_source",
        table,
        ScoredInput::All,
        vec!["id".into(), "payload".into()],
        Some("id".into()),
        None,
    );
    assert_eq!(retained.output_ordering()[0].position, 0);
}

#[test]
fn recheck_pins_clear_primary_key_ordering_when_their_order_is_not_ascending() {
    let engine = crate::Engine::new();
    engine
        .sql(
            "CREATE TABLE pinned_order (id BIGINT PRIMARY KEY, payload TEXT)",
            &[],
        )
        .unwrap();
    let table = engine.require_table("pinned_order").unwrap();
    let source = ScoredDocumentSource::new(
        "pinned_order",
        table,
        ScoredInput::All,
        vec!["id".into()],
        Some("id".into()),
        None,
    )
    .with_recheck_pins(Some(Arc::new(vec![
        RecheckDoc {
            doc_id: 2,
            document: None,
        },
        RecheckDoc {
            doc_id: 1,
            document: None,
        },
    ])));

    assert!(source.output_ordering().is_empty());
}

#[test]
fn store_cursor_materializes_empty_projection_without_losing_rows() {
    let engine = crate::Engine::new();
    engine
        .sql(
            "CREATE TABLE presence_source (id BIGINT PRIMARY KEY, payload TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO presence_source (id, payload) VALUES (1, 'a'), (2, 'b')",
            &[],
        )
        .unwrap();
    let table = engine.require_table("presence_source").unwrap();
    let mut source = ScoredDocumentSource::new(
        "presence_source",
        table,
        ScoredInput::All,
        Vec::new(),
        None,
        None,
    );

    let rows = source.next_batch(16).unwrap();
    assert_eq!(rows, vec![ResultRow::new(), ResultRow::new()]);
}

#[test]
fn physical_cursor_exposes_qualified_alias_without_copying_the_value() {
    let engine = crate::Engine::new();
    engine
        .sql("CREATE TABLE alias_source (id BIGINT PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO alias_source (id) VALUES (7)", &[])
        .unwrap();
    let table = engine.require_table("alias_source").unwrap();
    let mut source = ScoredDocumentSource::new(
        "alias_source",
        table,
        ScoredInput::All,
        vec!["id".into()],
        Some("id".into()),
        None,
    )
    .with_qualifier("a");
    let schema = source.physical_schema().unwrap().clone();
    let rows = source.next_physical_batch(16).unwrap();

    assert_eq!(schema.columns(), ["id"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].fragment_count(), 1);
    assert!(rows[0].lock_origins().is_empty());
    assert_eq!(
        schema.view(&rows[0]).qualified_column("a", "id"),
        Some(&Value::Int(7))
    );
}

#[test]
fn locking_physical_cursor_attaches_the_requested_origin() {
    let engine = crate::Engine::new();
    engine
        .sql("CREATE TABLE locking_source (id BIGINT PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO locking_source (id) VALUES (7)", &[])
        .unwrap();
    let table = engine.require_table("locking_source").unwrap();
    let mut source = ScoredDocumentSource::new(
        "locking_source",
        table,
        ScoredInput::All,
        vec!["id".into()],
        Some("id".into()),
        None,
    )
    .with_qualifier("l")
    .with_lock_origin(Some((Arc::from("l"), Arc::from("locking_source"))));

    let rows = source.next_physical_batch(16).unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lock_origins().len(), 1);
    assert_eq!(rows[0].lock_origins()[0].qualifier.as_ref(), "l");
    assert_eq!(
        rows[0].lock_origins()[0].storage_name.as_ref(),
        "locking_source"
    );
    assert_eq!(rows[0].lock_origins()[0].doc_id, 7);
}
