//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public results retain producer metadata through output conversion and catalog changes.

use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::expr::SQLRecordView;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_result_descriptors_survive_output_catalog_rollback_and_reopen(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared-composites.db");
    let engine = super::addition::open(provider, &path);
    engine
        .sql("CREATE TYPE shaped_pair AS (id integer, body text)", &[])
        .unwrap();
    engine
        .sql("CREATE TABLE shaped_input (id integer, body text)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO shaped_input VALUES (1, 'one'), (2, 'two'), (3, NULL)",
            &[],
        )
        .unwrap();
    let sql = "SELECT ROW(id, body) AS anonymous, ROW(id, body)::shaped_pair AS named FROM shaped_input ORDER BY id";
    let original = engine.sql(sql, &[]).unwrap();
    let (Some(Value::Row(first)), Some(Value::Datum(named))) =
        (original.value_at(0, 0), original.value_at(0, 1))
    else {
        panic!("composite carriers")
    };
    for row in 1..3 {
        let (Some(Value::Row(other)), Some(Value::Datum(other_named))) =
            (original.value_at(row, 0), original.value_at(row, 1))
        else {
            panic!("composite carriers")
        };
        assert!(first
            .descriptor()
            .unwrap()
            .shares_storage_with(other.descriptor().unwrap()));
        assert_eq!(named.type_oid(), other_named.type_oid());
    }
    let control = ProductionControl::uncontrolled();
    let named_view = SQLRecordView::new(
        original.value_at(0, 1).unwrap(),
        original.column_types[1].as_ref(),
        Some(&engine),
        &control,
    )
    .unwrap();
    assert_eq!(
        uqa_sql::result::format_postgres_text(
            original.value_at(2, 1).unwrap(),
            original.column_types[1].as_ref().unwrap(),
            Some(&engine)
        )
        .unwrap(),
        "(3,)"
    );
    let serialized = serde_json::to_string(original.value_at(0, 1).unwrap()).unwrap();
    engine
        .sql("CREATE TABLE shaped_saved (p shaped_pair)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO shaped_saved SELECT ROW(id, body)::shaped_pair FROM shaped_input",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "ALTER TYPE shaped_pair RENAME ATTRIBUTE body TO changed",
            &[],
        )
        .unwrap();
    let during = engine.sql(sql, &[]).unwrap();
    let during = SQLRecordView::new(
        during.value_at(0, 1).unwrap(),
        during.column_types[1].as_ref(),
        Some(&engine),
        &control,
    )
    .unwrap();
    assert_eq!(
        during
            .descriptor()
            .unwrap()
            .attributes
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "changed"]
    );
    assert_eq!(
        named_view
            .descriptor()
            .unwrap()
            .attributes
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "body"]
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    let restored = engine.sql(sql, &[]).unwrap();
    assert_eq!(
        serde_json::to_string(restored.value_at(0, 1).unwrap()).unwrap(),
        serialized
    );
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        super::addition::open(provider, &path)
    };
    let persisted = engine
        .sql("SELECT p FROM shaped_saved ORDER BY (p).id", &[])
        .unwrap();
    for row in 0..3 {
        assert!(persisted
            .value_at(row, 0)
            .unwrap()
            .has_same_representation(original.value_at(row, 1).unwrap()));
    }
    assert_eq!(
        serde_json::to_string(original.value_at(0, 1).unwrap()).unwrap(),
        serialized
    );
}
