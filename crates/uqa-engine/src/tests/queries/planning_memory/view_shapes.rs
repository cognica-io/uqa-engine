//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::ProductionControl;
use uqa_sql::semantics::view_rewrite::{
    production::{view_query_shape_owned, OwnedViewQueryShape},
    NotUpdatableReason,
};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_view_shape_admission_preserves_rejection_order_and_recovers(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("view-shape-owners.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer, payload text); INSERT INTO items VALUES(1,'initial'); CREATE VIEW writable AS SELECT id, payload FROM items WHERE id > 0 WITH CASCADED CHECK OPTION", &[]).unwrap();
    let large = "x".repeat(4 << 20);
    engine.sql(&format!("CREATE VIEW large_view AS SELECT id, '{large}'::text AS payload FROM items; CREATE VIEW rejected_view AS SELECT DISTINCT id, '{large}'::text AS payload FROM items LIMIT 1"), &[]).unwrap();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let cancellation = uqa_core::CancellationToken::new();
    let control = ProductionControl::new(memory, retention.cancellation(), &cancellation);
    let services = engine.view_rewrite_context();
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let observed = allocation_counter::measure(|| {
        let ordinary =
            uqa_sql::semantics::view_rewrite::view_query_shape(services, "rejected_view")
                .unwrap()
                .unwrap();
        assert!(matches!(
            ordinary,
            uqa_sql::semantics::view_rewrite::ViewQueryShape::NotUpdatable(
                NotUpdatableReason::Distinct
            )
        ));
        let shape = view_query_shape_owned(services, "rejected_view", &control)
            .unwrap()
            .unwrap();
        assert!(matches!(
            shape,
            OwnedViewQueryShape::NotUpdatable(NotUpdatableReason::Distinct)
        ));
        let error = view_query_shape_owned(services, "large_view", &control)
            .err()
            .expect("view payload must be admitted before copying");
        assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    });
    assert!(
        observed.bytes_total < 256 << 10,
        "{provider:?}: {observed:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    let shape = view_query_shape_owned(services, "writable", &control)
        .unwrap()
        .unwrap();
    let OwnedViewQueryShape::Updatable(layer) = shape else {
        panic!("writable view")
    };
    assert_eq!(layer.columns.len(), 2);
    assert_eq!(
        layer.columns[0]
            .writable_source_column
            .as_ref()
            .map(|column| column.as_str()),
        Some("id")
    );
    assert_eq!(
        layer.columns[1]
            .writable_source_column
            .as_ref()
            .map(|column| column.as_str()),
        Some("payload")
    );
    assert!(memory.used() > before);
    drop(layer);
    assert_eq!(memory.used(), before);
    cancellation.cancel();
    let error = view_query_shape_owned(services, "writable", &control)
        .err()
        .expect("cancelled view analysis");
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(memory.used(), before);
    engine.sql("UPDATE writable SET payload = 'updated' WHERE id = 1; INSERT INTO writable VALUES(2,'inserted'); DELETE FROM writable WHERE id = 1", &[]).unwrap();
    assert_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_rows(&reopened);
        reopened.close().unwrap();
    }
}

fn assert_rows(engine: &Engine) {
    let result = engine.sql("SELECT id,payload FROM items", &[]).unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(2)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Str("inserted".into())));
}
