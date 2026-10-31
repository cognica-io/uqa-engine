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
fn native_automatic_view_queries_preserve_shadowed_aliases_across_all_mutations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("view-query-embedding.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer PRIMARY KEY, amount integer); INSERT INTO items VALUES(1,2),(2,5); CREATE VIEW v AS SELECT src.id, src.amount, (SELECT src.amount + v.n FROM (VALUES(10)) AS v(n)) AS computed FROM items AS src WHERE src.amount > 0", &[]).unwrap();
    // PostgreSQL 18.4 reference: the local v(n) shadows the mutation target while src remains correlated with each row image.
    for (sql, expected) in [
        ("UPDATE v SET amount = amount + 1 WHERE computed = 12 RETURNING id,amount,computed", vec![[1,3,13]]),
        ("INSERT INTO v(id,amount) VALUES(3,7) RETURNING id,amount,computed", vec![[3,7,17]]),
        ("DELETE FROM v WHERE computed = 15 RETURNING id,amount,computed", vec![[2,5,15]]),
        ("MERGE INTO v USING (VALUES(1,20),(4,9)) AS source(id,amount) ON v.id = source.id WHEN MATCHED THEN UPDATE SET amount = source.amount WHEN NOT MATCHED THEN INSERT(id,amount) VALUES(source.id,source.amount) RETURNING v.id,v.amount,v.computed", vec![[1,20,30],[4,9,19]]),
    ] {
        let result = engine.sql(sql, &[]).unwrap_or_else(|error| panic!("{provider:?}: {sql}: {error}"));
        assert_eq!(result.rows.len(), expected.len(), "{provider:?}: {sql}");
        for (row, expected) in expected.iter().enumerate() {
            for (column, value) in expected.iter().enumerate() {
                assert_eq!(result.value_at(row, column), Some(&Value::Int(*value)), "{provider:?}: {sql}");
            }
        }
    }
    assert_rows(&engine);
    engine.sql("BEGIN; SAVEPOINT keep_rows; UPDATE v SET amount = amount + 100 RETURNING computed; ROLLBACK TO keep_rows; COMMIT", &[]).unwrap();
    assert_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_rows(&reopened);
        reopened.close().unwrap();
    }
}

fn assert_rows(engine: &Engine) {
    let result = engine
        .sql("SELECT id,amount,computed FROM v ORDER BY id", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    for (row, expected) in [[1, 20, 30], [3, 7, 17], [4, 9, 19]].iter().enumerate() {
        for (column, value) in expected.iter().enumerate() {
            assert_eq!(result.value_at(row, column), Some(&Value::Int(*value)));
        }
    }
}
