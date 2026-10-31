//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::cte::materialize_statement_ctes;
use uqa_sql::plan::source_projection::CteOutputFilters;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_postponed_cte_scheduling_admits_before_copying_and_keeps_scope_atomic(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("postponed-cte-owners.db"));
    engine
        .sql("CREATE TABLE pending_target(payload text)", &[])
        .unwrap();
    let payload = "x".repeat(4 << 20);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(&format!(
        "WITH later AS (INSERT INTO pending_target VALUES ('{payload}') RETURNING length(payload)) SELECT 1"
    )).unwrap().remove(0)) else { panic!("query") };
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let context = engine.source_execution_context();
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    assert!(scope.statement_commands().is_none());
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(materialize_statement_ctes(
            context.ctes,
            &query.ctes,
            query.ctes.iter(),
            |control| Ok(control.finish(Vec::new(), control.empty_reservation())?),
            &[],
            &mut scope,
            &CteOutputFilters::new(),
        ));
    });
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(scope.statement_commands().is_none());
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    materialize_statement_ctes(
        context.ctes,
        &query.ctes,
        query.ctes.iter(),
        |control| Ok(control.finish(Vec::new(), control.empty_reservation())?),
        &[],
        &mut scope,
        &CteOutputFilters::new(),
    )
    .unwrap();
    assert!(scope.statement_commands().unwrap().modifies_with());
    assert!(memory.used() >= baseline + payload.len());
    drop(scope);
    for token in [resources.cancellation(), context.ctes.runtime.cancellation] {
        let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
        let before = memory.used();
        token.cancel();
        let error = materialize_statement_ctes(
            context.ctes,
            &query.ctes,
            query.ctes.iter(),
            |control| Ok(control.finish(Vec::new(), control.empty_reservation())?),
            &[],
            &mut scope,
            &CteOutputFilters::new(),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert!(scope.statement_commands().is_none());
        assert_eq!(memory.used(), before);
        token.reset();
    }
    let result = engine
        .sql("SELECT count(*) FROM pending_target", &[])
        .unwrap();
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(0)));
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_postponed_ctes_preserve_pg18_writes_triggers_and_rollback(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("postponed-cte-effects.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE pending_target(id integer PRIMARY KEY); CREATE TABLE pending_audit(id integer); CREATE FUNCTION pending_after_insert() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN INSERT INTO pending_audit VALUES(NEW.id); RETURN NEW; END$$; CREATE TRIGGER target_insert AFTER INSERT ON pending_target FOR EACH ROW EXECUTE FUNCTION pending_after_insert()", &[]).unwrap();
    let check_rows = |expected: &[i64]| {
        for table in ["pending_target", "pending_audit"] {
            let result = engine
                .sql(&format!("SELECT id FROM {table} ORDER BY id"), &[])
                .unwrap();
            assert_eq!(result.rows.len(), expected.len());
            for (index, value) in expected.iter().enumerate() {
                assert_eq!(
                    result.value_at(index, 0),
                    Some(&Value::Int(*value)),
                    "{provider:?} {table}"
                );
            }
        }
    };
    let output = engine.sql("WITH unused AS (INSERT INTO pending_target VALUES(11) RETURNING id) SELECT 7 WHERE false", &[]).unwrap();
    assert!(output.rows.is_empty());
    check_rows(&[11]);
    engine.sql("WITH first AS (INSERT INTO pending_target VALUES(12) RETURNING id), next AS (INSERT INTO pending_target SELECT id+1 FROM first RETURNING id) SELECT 3", &[]).unwrap();
    check_rows(&[11, 12, 13]);
    engine.sql("EXPLAIN (MEMORY, FORMAT JSON) WITH unused AS (INSERT INTO pending_target VALUES(14) RETURNING id) SELECT 4", &[]).unwrap();
    check_rows(&[11, 12, 13]);
    engine.sql("BEGIN; SAVEPOINT retained", &[]).unwrap();
    let error = engine.sql("WITH unused AS (INSERT INTO pending_target VALUES(15) RETURNING id) SELECT 1/(id-id) FROM pending_target WHERE id=11", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    engine.sql("ROLLBACK TO retained", &[]).unwrap();
    check_rows(&[11, 12, 13]);
    engine
        .sql(
            "WITH unused AS (INSERT INTO pending_target VALUES(16) RETURNING id) SELECT 6; COMMIT",
            &[],
        )
        .unwrap();
    check_rows(&[11, 12, 13, 16]);
    engine.close().unwrap();
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        for table in ["pending_target", "pending_audit"] {
            let result = reopened
                .sql(&format!("SELECT id FROM {table} ORDER BY id"), &[])
                .unwrap();
            assert_eq!(result.rows.len(), 4);
            for (index, value) in [11, 12, 13, 16].into_iter().enumerate() {
                assert_eq!(result.value_at(index, 0), Some(&Value::Int(value)));
            }
        }
        reopened.close().unwrap();
    }
}
