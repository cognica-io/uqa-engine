//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Proves that catalogue statistics drive `DPccp`'s `SourcePlan` order and that
//! the reordered physical source produces the same SQL rows.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_core::Value;
use uqa_engine::{Engine, SQLScalarFunction};
use uqa_sql::SQLError;

struct ObserveEqual {
    calls: Arc<AtomicUsize>,
}

impl SQLScalarFunction for ObserveEqual {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        let [left, right] = args else {
            return Err(SQLError::BadArity {
                name: "observe_equal".into(),
                expected: "2 arguments".into(),
                actual: args.len(),
            });
        };
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Bool(left == right))
    }
}

fn values(rows: impl Iterator<Item = String>) -> String {
    rows.collect::<Vec<_>>().join(",")
}

#[test]
fn engine_uses_dpccp_source_order_and_preserves_join_results() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE dp_a (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE TABLE dp_b (id INTEGER PRIMARY KEY, a_id INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE dp_c (id INTEGER PRIMARY KEY, b_id INTEGER)",
            &[],
        )
        .unwrap();

    engine
        .sql(
            &format!(
                "INSERT INTO dp_a (id) VALUES {}",
                values((1..=200).map(|id| format!("({id})")))
            ),
            &[],
        )
        .unwrap();
    engine
        .sql(
            &format!(
                "INSERT INTO dp_b (id, a_id) VALUES {}",
                values((1..=20).map(|id| format!("({id}, {id})")))
            ),
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO dp_c (id, b_id) VALUES (1, 1), (2, 2), (3, 2)",
            &[],
        )
        .unwrap();
    for table in ["dp_a", "dp_b", "dp_c"] {
        engine.sql(&format!("ANALYZE {table}"), &[]).unwrap();
    }

    let query = "SELECT a.id AS id \
                 FROM dp_a AS a \
                 JOIN dp_b AS b ON a.id = b.a_id \
                 JOIN dp_c AS c ON b.id = c.b_id AND c.id <= b.id \
                 ORDER BY a.id";
    let explain = engine.sql(&format!("EXPLAIN {query}"), &[]).unwrap();
    let plan = explain
        .rows
        .iter()
        .filter_map(|row| match row.get("plan") {
            Some(Value::Str(line)) => Some(line.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let b_pos = plan.find("name: \"dp_b\"").expect("b scan in plan");
    let c_pos = plan.find("name: \"dp_c\"").expect("c scan in plan");
    let a_pos = plan.find("name: \"dp_a\"").expect("a scan in plan");
    assert!(
        b_pos < c_pos && c_pos < a_pos,
        "DPccp should join the selective b-c edge before a: {plan}"
    );
    assert_eq!(
        plan.matches("strategy: Hash").count(),
        2,
        "every DPccp equijoin must retain its executable hash strategy: {plan}"
    );

    let result = engine.sql(query, &[]).unwrap();
    let ids = result
        .rows
        .iter()
        .map(|row| row.get("id").cloned().expect("projected id"))
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![Value::Int(1), Value::Int(2)]);
}

#[test]
fn dpccp_does_not_move_a_volatile_join_predicate() {
    let engine = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function(
            "observe_equal",
            ObserveEqual {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    engine
        .sql("CREATE TABLE va (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE TABLE vb (id INTEGER PRIMARY KEY, a_id INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE vc (id INTEGER PRIMARY KEY, b_id INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            &format!(
                "INSERT INTO va (id) VALUES {}",
                values((1..=200).map(|id| format!("({id})")))
            ),
            &[],
        )
        .unwrap();
    engine
        .sql(
            &format!(
                "INSERT INTO vb (id, a_id) VALUES {}",
                values((1..=20).map(|id| format!("({id}, {id})")))
            ),
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO vc (id, b_id) VALUES (1, 1), (2, 2), (3, 2)",
            &[],
        )
        .unwrap();
    for table in ["va", "vb", "vc"] {
        engine.sql(&format!("ANALYZE {table}"), &[]).unwrap();
    }

    let result = engine
        .sql(
            "SELECT a.id AS id
             FROM va AS a
             JOIN vb AS b ON observe_equal(a.id, b.a_id)
             JOIN vc AS c ON b.id = c.b_id
             ORDER BY c.id",
            &[],
        )
        .unwrap();

    assert_eq!(result.rows.len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 4_000);
}
