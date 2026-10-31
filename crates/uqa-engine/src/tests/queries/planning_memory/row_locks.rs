//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{plan::RelationalPlan, ScalarExpr};

#[rstest::rstest]
#[case::memory_source(Provider::Memory, "source")]
#[case::memory_predicate(Provider::Memory, "predicate")]
#[case::sqlite_native_source(Provider::SQLiteNative, "source")]
#[case::sqlite_native_predicate(Provider::SQLiteNative, "predicate")]
#[case::sqlite_kv_source(Provider::SQLiteKeyValue, "source")]
#[case::sqlite_kv_predicate(Provider::SQLiteKeyValue, "predicate")]
#[case::redb_source(Provider::Redb, "source")]
#[case::redb_predicate(Provider::Redb, "predicate")]
fn native_row_lock_targets_admit_inputs_and_retire_resolved_metadata(
    #[case] provider: Provider,
    #[case] input: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("row-lock-targets.db"));
    engine
        .sql(
            "CREATE TABLE lock_inputs(id integer); INSERT INTO lock_inputs VALUES(7)",
            &[],
        )
        .unwrap();
    let sql = match input {
        "source" => "SELECT id FROM lock_inputs FOR UPDATE",
        _ => "SELECT a.id FROM lock_inputs a LEFT JOIN lock_inputs b ON a.id=b.id WHERE b.id>0 FOR UPDATE OF a",
    };
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("query block")
    };
    let payload = "x".repeat(4 << 20);
    if input == "source" {
        let Some(uqa_sql::plan::SourcePlan::Table { alias, .. }) = &mut block.from else {
            panic!("table source")
        };
        *alias = Some(payload);
    } else {
        block.r#where = Some(ScalarExpr::And(vec![
            block.r#where.take().unwrap(),
            ScalarExpr::IsNull {
                expr: Box::new(ScalarExpr::Literal(Value::Str(payload))),
                negated: true,
            },
        ]));
    }
    let scope = crate::capabilities::query_scope::new_for_current_routine(&engine).unwrap();
    let context = engine.row_lock_context();
    let resolve = || {
        uqa_execution::query::locking::resolve_row_locks(
            context,
            block.from.as_ref().unwrap(),
            &block.locking,
            block.r#where.as_ref(),
            &[],
            &scope,
        )
    };
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| result = Some(resolve()));
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?} {input}: {allocation:?}"
    );
    assert_eq!(
        result.unwrap().expect_err("admission rejection").sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    let targets = resolve().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].storage_name, "public.lock_inputs");
    if input == "source" {
        assert_eq!(targets[0].qualifier.len(), 4 << 20);
        assert!(targets[0].qualifier.bytes().all(|byte| byte == b'x'));
        assert!(memory.used() >= before + (8 << 20));
    } else {
        assert_eq!(targets[0].qualifier, "a");
    }
    drop(targets);
    assert_eq!(memory.used(), before);
    for token in [resources.cancellation(), context.cancellation] {
        token.cancel();
        assert_eq!(resolve().err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), before);
        token.reset();
    }
    drop(scope);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

// Independent PostgreSQL 18.4 results are retained in tests/parity/pg18/row_lock_planning_owners.expected.txt.
#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_row_lock_planning_preserves_pg18_null_rejection_views_and_recovery(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("row-lock-observation.db"));
    let resources = engine.query_retention_control().unwrap();
    engine.sql("BEGIN; CREATE TEMP TABLE lock_left(id integer); CREATE TEMP TABLE lock_right(id integer); INSERT INTO lock_left VALUES(1),(2); INSERT INTO lock_right VALUES(1); CREATE FUNCTION pg_temp.lock_strict(integer) RETURNS integer LANGUAGE sql IMMUTABLE STRICT AS $$SELECT $1$$; CREATE TEMP VIEW lock_view AS SELECT id FROM lock_left", &[]).unwrap();
    for sql in [
        "SELECT a.id FROM lock_left a LEFT JOIN lock_right b ON a.id=b.id WHERE b.id>0 FOR UPDATE OF b",
        "SELECT a.id FROM lock_left a LEFT JOIN lock_right b ON a.id=b.id WHERE pg_temp.lock_strict(b.id)>0 FOR UPDATE OF b",
        "SELECT a.id FROM lock_left a FULL JOIN lock_right b ON a.id=b.id WHERE a.id IS NOT NULL AND b.id IS NOT NULL FOR UPDATE OF a,b",
        "SELECT a.id FROM lock_left a RIGHT JOIN lock_right b ON a.id=b.id WHERE a.id>0 FOR UPDATE OF a",
        "SELECT j.aid AS id FROM (lock_left a LEFT JOIN lock_right b ON a.id=b.id) AS j(aid,bid) WHERE j.bid>0 FOR UPDATE",
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), 1, "{sql}");
        assert_eq!(result.rows[0]["id"], Value::Int(1), "{sql}");
    }
    for sql in [
        "SELECT id FROM lock_view ORDER BY id FOR SHARE",
        "SELECT id FROM lock_view ORDER BY id FOR UPDATE",
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row["id"].clone())
                .collect::<Vec<_>>(),
            vec![Value::Int(1), Value::Int(2)]
        );
    }
    engine.sql("SAVEPOINT invalid", &[]).unwrap();
    for sql in [
        "SELECT a.id FROM lock_left a LEFT JOIN lock_right b ON a.id=b.id FOR UPDATE OF b",
        "SELECT a.id FROM lock_left a LEFT JOIN lock_right b ON a.id=b.id WHERE coalesce(b.id,1)>0 FOR UPDATE OF b",
        "SELECT j.id FROM (lock_left a FULL JOIN lock_right b USING(id)) AS j WHERE j.id IS NOT NULL FOR UPDATE",
    ] {
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some("0A000"), "{sql}");
        engine.sql("ROLLBACK TO invalid", &[]).unwrap();
    }
    let result = engine
        .sql("SELECT id FROM lock_view ORDER BY id FOR UPDATE", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    engine.sql("ROLLBACK", &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(resources.memory().used(), 0);
}
