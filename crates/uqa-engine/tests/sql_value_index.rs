//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Value-index correctness: indexed scalar predicates must return
//! exactly what the evaluated scan returns, across inserts, updates,
//! deletes, truncates, and persistent reopens. `indexed` carries a
//! PRIMARY KEY plus btree indexes; `shadow` is the same data with no
//! indexable columns, so every query there takes the scan path.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLResult;

fn ids(result: &SQLResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(v)) => *v,
            other => panic!("unexpected id value: {other:?}"),
        })
        .collect()
}

fn setup(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE indexed (id INTEGER PRIMARY KEY, qty INTEGER, owner TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE shadow (id INTEGER, qty INTEGER, owner TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX indexed_qty ON indexed USING btree (qty)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE INDEX indexed_owner ON indexed USING btree (owner)",
            &[],
        )
        .unwrap();
    let mut values = Vec::new();
    for i in 1..=500_i64 {
        let owner = format!("owner{}", i % 7);
        values.push(format!("({i}, {}, '{owner}')", i % 50));
    }
    for table in ["indexed", "shadow"] {
        engine
            .sql(
                &format!(
                    "INSERT INTO {table} (id, qty, owner) VALUES {}",
                    values.join(", ")
                ),
                &[],
            )
            .unwrap();
    }
}

fn assert_same(engine: &Engine, predicate: &str) {
    let indexed = engine
        .sql(
            &format!("SELECT id FROM indexed WHERE {predicate} ORDER BY id"),
            &[],
        )
        .unwrap();
    let shadow = engine
        .sql(
            &format!("SELECT id FROM shadow WHERE {predicate} ORDER BY id"),
            &[],
        )
        .unwrap();
    assert_eq!(
        ids(&indexed),
        ids(&shadow),
        "index and scan disagree for `{predicate}`"
    );
}

const PREDICATES: &[&str] = &[
    "qty = 25",
    "qty = 0",
    "qty <> 25",
    "qty > 45",
    "qty >= 45",
    "qty < 3",
    "qty <= 3",
    "qty BETWEEN 10 AND 12",
    "qty IN (1, 2, 3)",
    "qty IS NULL",
    "qty IS NOT NULL",
    "owner = 'owner3'",
    "owner = 'missing'",
    "qty = 25 AND owner = 'owner4'",
    "qty = 25 OR qty = 26",
    "id = 250",
    "id BETWEEN 100 AND 105",
];

fn assert_all_predicates(engine: &Engine) {
    for predicate in PREDICATES {
        assert_same(engine, predicate);
    }
}

#[test]
fn indexed_predicates_match_scan_across_writes() {
    let engine = Engine::new();
    setup(&engine);
    // First pass builds the lazy indexes; second pass reads them.
    assert_all_predicates(&engine);
    assert_all_predicates(&engine);

    // Mutate through every write shape and re-compare.
    for table in ["indexed", "shadow"] {
        engine
            .sql(&format!("UPDATE {table} SET qty = 999 WHERE id = 250"), &[])
            .unwrap();
        engine
            .sql(
                &format!("UPDATE {table} SET owner = 'owner3' WHERE qty = 7"),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("UPDATE {table} SET qty = NULL WHERE id = 10"), &[])
            .unwrap();
        engine
            .sql(&format!("DELETE FROM {table} WHERE qty = 11"), &[])
            .unwrap();
        engine
            .sql(
                &format!("INSERT INTO {table} (id, qty, owner) VALUES (601, 25, 'owner9')"),
                &[],
            )
            .unwrap();
    }
    assert_same(&engine, "qty = 999");
    assert_same(&engine, "qty = 25");
    assert_same(&engine, "qty IS NULL");
    assert_same(&engine, "qty = 11");
    assert_same(&engine, "owner = 'owner9'");
    assert_all_predicates(&engine);

    // Aggregate over an indexed filter agrees with the scan table.
    let indexed = engine
        .sql("SELECT count(*) AS n FROM indexed WHERE qty = 25", &[])
        .unwrap();
    let shadow = engine
        .sql("SELECT count(*) AS n FROM shadow WHERE qty = 25", &[])
        .unwrap();
    assert_eq!(indexed.rows[0].get("n"), shadow.rows[0].get("n"));
}

#[test]
fn truncate_resets_indexes() {
    let engine = Engine::new();
    setup(&engine);
    assert_all_predicates(&engine);
    engine.sql("TRUNCATE indexed", &[]).unwrap();
    engine.sql("TRUNCATE shadow", &[]).unwrap();
    assert_all_predicates(&engine);
    for table in ["indexed", "shadow"] {
        engine
            .sql(
                &format!("INSERT INTO {table} (id, qty, owner) VALUES (1, 5, 'owner1')"),
                &[],
            )
            .unwrap();
    }
    assert_same(&engine, "qty = 5");
    assert_same(&engine, "qty IS NOT NULL");
}

#[test]
fn persistent_reopen_keeps_index_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("value-index.db");
    {
        let engine = Engine::open(&path).unwrap();
        setup(&engine);
        assert_all_predicates(&engine);
        engine
            .sql("UPDATE indexed SET qty = 999 WHERE id = 250", &[])
            .unwrap();
        engine
            .sql("UPDATE shadow SET qty = 999 WHERE id = 250", &[])
            .unwrap();
    }
    let engine = Engine::open(&path).unwrap();
    assert_same(&engine, "qty = 999");
    assert_all_predicates(&engine);

    // Writes after reopen keep the rebuilt indexes in sync.
    for table in ["indexed", "shadow"] {
        engine
            .sql(&format!("DELETE FROM {table} WHERE qty = 25"), &[])
            .unwrap();
    }
    assert_same(&engine, "qty = 25");
    assert_all_predicates(&engine);
}
