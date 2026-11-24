//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use serde_json::Value as Json;
use uqa_core::Value;

const SCALAR: &str = "SELECT b.v FROM b WHERE b.id = (SELECT a.b_id FROM a WHERE a.id = 'a77')";
const MEMBERSHIP: &str =
    "SELECT b.v FROM b WHERE b.id IN (SELECT a.b_id FROM a WHERE a.id = 'a77')";
const JOIN: &str = "SELECT b.v FROM a JOIN b ON b.id = a.b_id WHERE a.id = 'a77'";
const CHAIN: &str =
    "SELECT b.v FROM s JOIN a ON a.id = s.a_id JOIN b ON b.id = a.b_id WHERE s.id = 's77'";

fn explain(engine: &Engine, query: &str) -> Json {
    let result = engine
        .sql(
            &format!("EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) {query}"),
            &[],
        )
        .unwrap_or_else(|err| panic!("{query}: {err}"));
    let Some(Value::Json(text)) = result.value_at(0, 0) else {
        panic!("missing JSON plan for {query}");
    };
    let document: Json = serde_json::from_str(text).unwrap();
    document[0].clone()
}

fn check_lookup_before_analyze(engine: &Engine) {
    let doc = explain(
        engine,
        "SELECT b.v FROM b WHERE b.id = (SELECT 'b77'::text)",
    );
    let relations = doc["Node Estimates"].as_array().unwrap();
    let target = relations
        .iter()
        .find(|node| node["Relation Name"] == "b")
        .unwrap();
    assert_eq!(target["Plan Rows"].as_f64(), Some(1.0), "{doc}");
    assert!(target["Total Cost"].as_f64().unwrap() < 64.0, "{doc}");
    let nodes = doc["Node Executions"].as_array().unwrap();
    for kind in ["Relation Scan", "Index Scan"] {
        let scans: Vec<_> = nodes
            .iter()
            .filter(|node| node["Node Type"] == kind)
            .collect();
        assert_eq!(scans.len(), 1, "{doc}");
        assert_eq!(scans[0]["Actual Rows"].as_f64(), Some(1.0), "{doc}");
        assert_eq!(scans[0]["Actual Loops"].as_f64(), Some(1.0), "{doc}");
    }
}

fn check_selective_plans(engine: &Engine) {
    for (query, relation_count, indexed_joins) in [
        (SCALAR, 2, 0),
        (MEMBERSHIP, 2, 0),
        (JOIN, 2, 1),
        (CHAIN, 3, 2),
        ("EXECUTE lookup_chain('s77')", 3, 2),
        (
            "SELECT v FROM b WHERE id=(SELECT b_id FROM a WHERE id='a77')",
            2,
            0,
        ),
        (
            "SELECT v FROM b WHERE id=(SELECT b_id FROM a WHERE id='a77') AND v <> 'absent'",
            2,
            0,
        ),
        (
            "SELECT v FROM b WHERE v=(SELECT 'v77'::text) AND id=(SELECT b_id FROM a WHERE id='a77')",
            2,
            0,
        ),
        (
            "SELECT v FROM b WHERE id IN (SELECT b_id FROM a WHERE id='a77') AND v=(SELECT 'v77'::text)",
            2,
            0,
        ),
    ] {
        let result = engine.sql(query, &[]).unwrap();
        assert_eq!(result.rows.len(), 1, "{query}");
        assert_eq!(
            result.value_at(0, 0),
            Some(&Value::Str("v77".into())),
            "{query}"
        );
        let document = explain(engine, query);
        let relations = document["Node Estimates"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["Node Type"] == "Relation")
            .collect::<Vec<_>>();
        assert_eq!(relations.len(), relation_count, "{query}: {relations:?}");
        for node in relations {
            let rows = node["Plan Rows"].as_f64().unwrap();
            let cost = node["Total Cost"].as_f64().unwrap();
            assert!(rows <= 1.0 && cost < 64.0, "{query}: {node}");
        }
        let operators = &document["Plan"]["UQA Plan"];
        assert_eq!(
            operators.to_string().matches("IndexNestedLoop").count(),
            indexed_joins,
            "{query}: {operators}"
        );
        let executed = document["Node Executions"].as_array().unwrap();
        if indexed_joins == 0 {
            assert!(
                executed
                    .iter()
                    .any(|op| op["Node Type"] == "Index Scan"
                        && op["Actual Rows"].as_f64() == Some(1.0)),
                "{query}: {executed:?}"
            );
        }
        let scans: Vec<_> = executed
            .iter()
            .filter(|op| op["Node Type"] == "Relation Scan")
            .collect();
        assert_eq!(scans.len(), relation_count, "{query}: {scans:?}");
        for scan in scans {
            assert_eq!(scan["Actual Rows"].as_f64(), Some(1.0), "{query}: {scan}");
            assert_eq!(scan["Actual Loops"].as_f64(), Some(1.0), "{query}: {scan}");
        }
    }
}

#[test]
fn analyzed_subquery_keys_share_the_indexed_columns_equality_domain() {
    use uqa_planner::statement_planning::PlannerStatisticsCatalog;
    use uqa_sql::catalog::index::lookup::{EqualityDomain, SubqueryKeys};
    use uqa_sql::plan::{ExecutablePlanOptimizer, RelationalPlan, UnifiedPlan};

    let engine = Engine::new();
    engine.sql("CREATE TABLE a(id text PRIMARY KEY, b_id text); CREATE TABLE b(id text PRIMARY KEY, v text)", &[]).unwrap();
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    for sql in [SCALAR, MEMBERSHIP] {
        let input = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        let executable = engine.plan_for_execution(input, &[]).unwrap();
        let UnifiedPlan::Query(query) = executable.as_ref() else {
            panic!("expected query")
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block")
        };
        let filter = block.r#where.as_ref().unwrap();
        let (field, key) =
            SubqueryKeys::predicate(filter).unwrap_or_else(|| panic!("{sql}: {filter:?}"));
        let column = engine
            .lookup_column("b", field)
            .unwrap()
            .expect("indexed base column");
        assert!(column.unique);
        assert_eq!(column.domain, EqualityDomain::Text);
        let inner = &block.subqueries[key.slot()];
        assert_eq!(
            engine.subquery_key_domain(inner, &control).unwrap(),
            Some(column.domain),
            "{sql}: {inner:?}"
        );
        assert!(
            engine
                .placed_filters(block, block.from.as_ref().unwrap(), &control)
                .unwrap()
                .is_some(),
            "{sql}: {filter:?}"
        );
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn selective_runtime_keys_bound_relation_access(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("runtime-keys.db"));
    super::pause_automatic_statistics(&engine);
    engine
        .sql(
            "CREATE TABLE a (id text PRIMARY KEY, b_id text NOT NULL);
        CREATE TABLE b (id text PRIMARY KEY, v text NOT NULL);
        CREATE TABLE s (id text PRIMARY KEY, a_id text NOT NULL);
        INSERT INTO a SELECT 'a' || i, 'b' || i FROM generate_series(1,256) AS g(i);
        INSERT INTO b SELECT 'b' || i, 'v' || i FROM generate_series(1,256) AS g(i);
        INSERT INTO s SELECT 's' || i, 'a' || i FROM generate_series(1,256) AS g(i)",
            &[],
        )
        .unwrap();

    engine.sql(
        "SET plan_cache_mode=force_generic_plan;
         PREPARE lookup_chain(text) AS SELECT b.v FROM s JOIN a ON a.id=s.a_id JOIN b ON b.id=a.b_id WHERE s.id=$1",
        &[],
    ).unwrap();
    check_lookup_before_analyze(&engine);
    check_selective_plans(&engine);
    engine.sql("ANALYZE a; ANALYZE b; ANALYZE s", &[]).unwrap();
    check_selective_plans(&engine);
    engine
        .sql(
            "CREATE TABLE int_target(k bigint PRIMARY KEY);
        INSERT INTO int_target SELECT i FROM generate_series(-1,254) AS g(i);
        ANALYZE int_target",
            &[],
        )
        .unwrap();
    for value in [-1, 77] {
        let sql = format!("SELECT k FROM int_target WHERE k=(SELECT {value}::bigint)");
        assert_eq!(
            engine.sql(&sql, &[]).unwrap().value_at(0, 0),
            Some(&Value::Int(value))
        );
        let doc = explain(&engine, &sql);
        let nodes = doc["Node Executions"].as_array().unwrap();
        assert!(
            nodes.iter().any(|node| node["Node Type"] == "Index Scan"
                && node["Actual Rows"].as_f64() == Some(1.0)),
            "{sql}: {doc}"
        );
    }
    let broad = explain(&engine, "SELECT count(*) FROM a JOIN b ON b.id=a.b_id");
    assert!(
        broad["Plan"]["UQA Plan"]
            .to_string()
            .contains("strategy: Hash"),
        "{broad}"
    );
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn runtime_lookups_preserve_sql_comparisons_and_statement_visibility(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("lookup-semantics.db"));
    engine.sql("CREATE TABLE target(k text, v integer); CREATE UNIQUE INDEX target_lookup ON target(k);
        CREATE TABLE keys(k text);
        INSERT INTO target VALUES ('a',1),('b',2),('c',3),(NULL,4);
        INSERT INTO keys VALUES ('a'),('a'),('b'),(NULL)", &[]).unwrap();
    let check = |query: &str, expected: &[i64]| {
        let rows = engine
            .sql(query, &[])
            .unwrap_or_else(|error| panic!("{query}: {error}"));
        let values: Vec<_> = (0..rows.rows.len())
            .map(|i| rows.value_at(i, 0).unwrap().clone())
            .collect();
        assert_eq!(
            values,
            expected.iter().copied().map(Value::Int).collect::<Vec<_>>(),
            "{query}"
        );
    };
    check(
        "SELECT v FROM target WHERE k IN (SELECT k FROM keys) ORDER BY v",
        &[1, 2],
    );
    check(
        "SELECT v FROM target WHERE k NOT IN (SELECT k FROM keys) ORDER BY v",
        &[],
    );
    check(
        "SELECT target.v FROM keys JOIN target ON keys.k=target.k ORDER BY target.v",
        &[1, 1, 2],
    );
    check("SELECT v FROM target t WHERE t.k=(SELECT q.k FROM keys q WHERE q.k=t.k LIMIT 1) ORDER BY v", &[1, 2]);
    check(
        "SELECT v FROM target t WHERE t.k IN (SELECT q.k FROM keys q WHERE q.k=t.k) ORDER BY v",
        &[1, 2],
    );
    check(
        "SELECT v FROM target t WHERE t.k=(SELECT k FROM keys WHERE false)",
        &[],
    );
    check(
        "SELECT v FROM target t WHERE t.k=(SELECT k FROM keys WHERE k IS NULL)",
        &[],
    );
    let cardinality = "SELECT v FROM target WHERE k=(SELECT k FROM keys)";
    engine.sql(&format!("EXPLAIN {cardinality}"), &[]).unwrap();
    check(&format!("{cardinality} LIMIT 0"), &[]);
    assert_eq!(
        engine.sql(cardinality, &[]).unwrap_err().sqlstate(),
        Some("21000")
    );
    engine.sql("SET plan_cache_mode=force_generic_plan; PREPARE key_lookup(text) AS SELECT v FROM target WHERE k=(SELECT $1)", &[]).unwrap();
    check("EXECUTE key_lookup('a')", &[1]);
    check("EXECUTE key_lookup('b')", &[2]);
    engine.sql("BEGIN; UPDATE target SET k='d' WHERE v=1; INSERT INTO keys VALUES('d'); DELETE FROM target WHERE v=2", &[]).unwrap();
    check(
        "SELECT v FROM target WHERE k IN (SELECT k FROM keys) ORDER BY v",
        &[1],
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    check(
        "SELECT v FROM target WHERE k IN (SELECT k FROM keys) ORDER BY v",
        &[1, 2],
    );
    engine
        .sql(
            "INSERT INTO keys SELECT 'other' || i FROM generate_series(1,100) g(i)",
            &[],
        )
        .unwrap();
    check(
        "SELECT v FROM target WHERE k IN (SELECT k FROM keys) ORDER BY v",
        &[1, 2],
    );
    engine.sql("CREATE TABLE empty_target(k text PRIMARY KEY); CREATE TABLE tagged(v integer, tag integer, k text UNIQUE);
        INSERT INTO tagged VALUES (10,1,'a'),(20,2,'b'); ANALYZE tagged", &[]).unwrap();
    assert_eq!(
        engine
            .sql(
                "SELECT k FROM empty_target WHERE k=(SELECT k FROM keys)",
                &[]
            )
            .unwrap_err()
            .sqlstate(),
        Some("21000")
    );
    check("SELECT t.v FROM (SELECT * FROM target WHERE v < 3) q JOIN tagged t ON t.k=q.k AND t.tag=q.v ORDER BY t.v", &[10,20]);
    check("WITH tagged(k,v) AS (VALUES ('a',8),('a',9)) SELECT tagged.v FROM target t JOIN tagged ON t.k=tagged.k WHERE t.k='a' ORDER BY tagged.v", &[8,9]);
    engine
        .sql(
            "CREATE TABLE partial_target(k text, active boolean, v integer);
        CREATE UNIQUE INDEX active_key ON partial_target(k) WHERE active;
        INSERT INTO partial_target VALUES ('a',true,1),('a',false,2)",
            &[],
        )
        .unwrap();
    check(
        "SELECT v FROM partial_target WHERE k=(SELECT k FROM target WHERE v=1) ORDER BY v",
        &[1, 2],
    );
    engine.close().unwrap();
}
