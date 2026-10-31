//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_lexical_owners_preserve_pg18_shadowing_recursion_quotes_and_lateral(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("lexical-inputs.db"));
    // PostgreSQL 18.4 reference: uqa-lexical-pg18.sql, SHA-256 recorded with this ownership unit.
    let cases: &[(&str, &[&[i64]])] = &[
        ("WITH q AS (SELECT 7 AS n) SELECT r.n AS captured, r.shadow FROM (WITH r AS (SELECT n FROM q), q AS (SELECT 9 AS n) SELECT r.n, q.n AS shadow FROM r, q) AS r", &[&[7, 9]]),
        ("WITH q AS (SELECT 7 AS n) SELECT * FROM (WITH q AS (SELECT n + 1 AS n FROM q) SELECT n FROM q) AS nested", &[&[8]]),
        ("WITH RECURSIVE q(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM q WHERE n<3), r AS (SELECT sum(n) AS n FROM q) SELECT n FROM r", &[&[6]]),
        ("WITH \"Q.CTE\" AS (SELECT 7 AS n), \"a\"\"b\" AS (SELECT n+1 AS n FROM \"Q.CTE\") SELECT n FROM \"a\"\"b\"", &[&[8]]),
        ("SELECT v.n, q.n FROM (VALUES(1),(2),(3)) AS v(n), LATERAL (WITH q AS MATERIALIZED (SELECT v.n+10 AS n) SELECT n FROM q) AS q ORDER BY v.n", &[&[1,11], &[2,12], &[3,13]]),
    ];
    for (sql, expected) in cases {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), expected.len(), "{provider:?}: {sql}");
        for (row, values) in expected.iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                assert_eq!(
                    result.value_at(row, column),
                    Some(&Value::Int(*value)),
                    "{provider:?}: {sql}"
                );
            }
        }
    }
    engine.close().unwrap();
}
