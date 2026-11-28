//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, SessionPortalData};
use uqa_core::Value;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn forward_cursor_delivers_only_requested_rows_without_materializing(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("forward"));
    let sql = |statement| engine.sql(statement, &[]);
    sql("CREATE SEQUENCE portal_sequence").unwrap();
    sql("BEGIN").unwrap();
    sql("DECLARE seq_cur NO SCROLL CURSOR FOR SELECT n, nextval('portal_sequence') AS seq FROM generate_series(1, 15) AS g(n)").unwrap();
    assert_eq!(sql("MOVE FORWARD 0 FROM seq_cur").unwrap().affected_rows, 0);
    let result = sql("FETCH 4 FROM seq_cur").unwrap();
    assert_eq!(result.rows.len(), 4);
    for (index, row) in result.rows.iter().enumerate() {
        let value = Value::Int(i64::try_from(index + 1).unwrap());
        assert_eq!(row["n"], value);
        assert_eq!(row["seq"], value);
    }
    assert_eq!(
        sql("SELECT currval('portal_sequence') AS n").unwrap().rows[0]["n"],
        Value::Int(4)
    );
    assert!(
        matches!(
            engine.session.portals.lock()["seq_cur"].data,
            SessionPortalData::Streaming {
                materialized: None,
                ..
            }
        ),
        "a forward-only cursor retained its output file"
    );
    assert_eq!(sql("MOVE FORWARD 5 FROM seq_cur").unwrap().affected_rows, 5);
    assert_eq!(
        sql("MOVE RELATIVE 0 FROM seq_cur").unwrap().affected_rows,
        1
    );
    let row = sql("FETCH ABSOLUTE 12 FROM seq_cur").unwrap();
    assert_eq!(row.rows[0]["seq"], Value::Int(12));
    let rest = sql("FETCH ALL FROM seq_cur").unwrap();
    assert_eq!(rest.rows.len(), 3);
    assert_eq!(rest.rows[2]["seq"], Value::Int(15));
    assert_eq!(sql("MOVE FORWARD 0 FROM seq_cur").unwrap().affected_rows, 0);
    assert!(sql("FETCH NEXT FROM seq_cur").unwrap().rows.is_empty());
    sql("ROLLBACK").unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn forward_cursor_promoted_by_a_procedure_preserves_unread_rows_and_sequence_calls(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("pinned"));
    engine
        .sql(
            "CREATE SEQUENCE loop_sequence; CREATE TABLE loop_output (n integer, value bigint)",
            &[],
        )
        .unwrap();
    engine.sql("DO $body$ DECLARE item record; BEGIN
        FOR item IN SELECT n, nextval('loop_sequence') AS value FROM generate_series(1, 6) AS src(n) LOOP
          INSERT INTO loop_output VALUES (item.n, item.value);
          IF item.n = 2 THEN COMMIT; END IF;
        END LOOP; END $body$", &[]).unwrap();
    let result = engine
        .sql("SELECT n, value FROM loop_output ORDER BY n", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 6);
    for (index, row) in result.rows.iter().enumerate() {
        let expected = Value::Int(i64::try_from(index + 1).unwrap());
        assert_eq!(row["n"], expected);
        assert_eq!(row["value"], expected);
    }
    assert_eq!(
        engine
            .sql("SELECT currval('loop_sequence') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(6)
    );
}
