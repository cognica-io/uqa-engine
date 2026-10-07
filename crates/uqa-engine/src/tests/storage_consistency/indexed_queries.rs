//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar candidates must retain the selected query view through every source shape.

use super::*;
use std::sync::atomic::Ordering;

#[rstest::rstest]
#[case::plain("SELECT id, body FROM probe WHERE qty = $1 + 1")]
#[case::alias("SELECT p.id, p.body FROM probe AS p WHERE p.qty = $1 + 1")]
#[case::column_alias("SELECT p.i AS id, p.b AS body FROM probe AS p(i,q,b) WHERE p.q = $1 + 1")]
#[case::join(
    "SELECT p.id, p.body FROM probe AS p JOIN (VALUES (1)) AS v(n) ON true WHERE p.qty = $1 + 1"
)]
fn scalar_index_candidates_do_not_scan_unrelated_rows(#[case] sql: &str) {
    for count in [32, 128] {
        let engine = Engine::new();
        engine.sql("CREATE TABLE probe (id integer, qty integer, body text); CREATE INDEX probe_qty ON probe(qty)", &[]).unwrap();
        engine.sql(&format!("INSERT INTO probe SELECT i * 10, i, repeat('x',20000) FROM generate_series(1,{count}) g(i)"), &[]).unwrap();
        engine
            .sql("SELECT id FROM probe WHERE qty = 1", &[])
            .unwrap();
        let probe = PortalSnapshotProbeStore::from_table(&engine, "probe");
        let enumerations = Arc::clone(&probe.doc_id_calls);
        let fields = Arc::clone(&probe.field_reads);
        let rows = Arc::clone(&probe.row_reads);
        *engine
            .table("probe")
            .unwrap()
            .unwrap()
            .document_store
            .write() = Box::new(probe);
        for parameter in [3, 999] {
            enumerations.store(0, Ordering::Relaxed);
            fields.store(0, Ordering::Relaxed);
            rows.store(0, Ordering::Relaxed);
            let result = engine
                .sql(sql, &[SQLParam::scalar(Value::Int(parameter))])
                .unwrap();
            let expected = if parameter == 3 {
                vec![Value::Int(40)]
            } else {
                vec![]
            };
            assert_eq!(
                result
                    .rows
                    .iter()
                    .map(|row| row["id"].clone())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                enumerations.load(Ordering::Relaxed),
                0,
                "{sql}: scanned {count} stored rows"
            );
            assert!(
                fields.load(Ordering::Relaxed) + rows.load(Ordering::Relaxed) <= result.rows.len(),
                "{sql}: read unrelated projected payloads"
            );
        }
    }
}
