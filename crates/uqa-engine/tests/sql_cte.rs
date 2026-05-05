//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `WITH` and `WITH RECURSIVE` common table expressions.

use uqa_core::Value;
use uqa_engine::Engine;

fn org_corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE org_chart (id INTEGER PRIMARY KEY, name TEXT, manager_id INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO org_chart (id, name, manager_id) VALUES \
         (1, 'ceo', 0), \
         (2, 'vp_eng', 1), \
         (3, 'vp_sales', 1), \
         (4, 'eng_director', 2), \
         (5, 'eng_lead', 4), \
         (6, 'engineer_a', 5), \
         (7, 'engineer_b', 5), \
         (8, 'sales_lead', 3)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn non_recursive_cte_aliases_query() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT, qty INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO items (id, label, qty) VALUES \
         (1, 'apple', 3), (2, 'banana', 7), (3, 'cherry', 2)",
        &[],
    )
    .unwrap();

    let r = eng
        .sql(
            "WITH high AS (SELECT id, label FROM items WHERE qty > 2) \
             SELECT id, label FROM high ORDER BY id",
            &[],
        )
        .unwrap();
    let labels: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("label") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, vec!["apple".to_string(), "banana".to_string()]);
}

#[test]
fn recursive_cte_traverses_org_tree() {
    let eng = org_corpus();
    let r = eng
        .sql(
            "WITH RECURSIVE org_tree AS ( \
                 SELECT id, name, 1 AS depth FROM org_chart WHERE manager_id = 0 \
                 UNION ALL \
                 SELECT o.id, o.name, t.depth + 1 \
                 FROM org_chart AS o \
                 INNER JOIN org_tree AS t ON o.manager_id = t.id \
             ) \
             SELECT name, depth FROM org_tree ORDER BY depth, name",
            &[],
        )
        .unwrap();
    let pairs: Vec<(String, i64)> = r
        .rows
        .iter()
        .filter_map(|row| match (row.get("name"), row.get("depth")) {
            (Some(Value::Str(n)), Some(Value::Int(d))) => Some((n.clone(), *d)),
            _ => None,
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("ceo".into(), 1),
            ("vp_eng".into(), 2),
            ("vp_sales".into(), 2),
            ("eng_director".into(), 3),
            ("sales_lead".into(), 3),
            ("eng_lead".into(), 4),
            ("engineer_a".into(), 5),
            ("engineer_b".into(), 5),
        ]
    );
}

#[test]
fn recursive_cte_terminates_on_empty_step() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE counter (id INTEGER PRIMARY KEY, n INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO counter (id, n) VALUES (1, 1)", &[])
        .unwrap();

    let r = eng
        .sql(
            "WITH RECURSIVE up AS ( \
                 SELECT n FROM counter \
                 UNION ALL \
                 SELECT n + 1 FROM up WHERE n < 5 \
             ) \
             SELECT n FROM up ORDER BY n",
            &[],
        )
        .unwrap();
    let ns: Vec<i64> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("n") {
            Some(Value::Int(v)) => Some(*v),
            _ => None,
        })
        .collect();
    assert_eq!(ns, vec![1, 2, 3, 4, 5]);
}
