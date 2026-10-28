//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::Value;
use uqa_planner::UnifiedPlan;

const JOIN: &str = "SELECT a.id FROM a JOIN b ON a.id = b.id JOIN c ON b.id = c.id";

fn populate(engine: &Engine) {
    engine.sql(
        "CREATE TABLE a (id INTEGER); CREATE TABLE b (id INTEGER); CREATE TABLE c (id INTEGER); \
         INSERT INTO a VALUES (7); INSERT INTO b VALUES (7); INSERT INTO c VALUES (7)",
        &[],
    ).unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_join_planning_shares_request_allowance_and_cancellation(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("join-owner.db"));
    populate(&engine);
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let input = UnifiedPlan::lower(uqa_sql::compile(JOIN).unwrap().remove(0));
    let expected =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input.clone())
            .unwrap();
    let before = memory.used();
    let full = memory.reserve(memory.limit() - before).unwrap();
    let error =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input.clone())
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    drop(full);
    assert_eq!(memory.used(), before);

    engine.cancel();
    let error =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input.clone())
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"), "{provider:?}: {error}");
    engine.reset_cancellation();
    let actual =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input).unwrap();
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    drop((actual, expected));
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_join_requests_recover_after_admission_failure(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("join-requests.db");
    let engine = provider.open(&path);
    populate(&engine);
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let query = format!("{JOIN} WHERE a.id = $1 AND a.id >= 0");
    let params = [uqa_sql::SQLParam::Scalar(Value::Int(7))];
    verify_request_recovery(&engine, &query, &params, &Value::Int(7), provider);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        let result = reopened.sql(JOIN, &[]).unwrap();
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(7)));
        drop(result);
        reopened.close().unwrap();
    }
}

fn verify_request_recovery(
    engine: &Engine,
    query: &str,
    params: &[uqa_sql::SQLParam],
    expected: &Value,
    provider: Provider,
) {
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    for mode in ["force_custom_plan", "force_generic_plan"] {
        engine
            .sql(&format!("SET plan_cache_mode = {mode}"), &[])
            .unwrap();
        let prepared = engine.prepare_statement(query, &[]).unwrap();
        for path in 0..3 {
            engine.sql("BEGIN; SAVEPOINT planning_retry", &[]).unwrap();
            let full = memory.reserve(memory.limit() - memory.used()).unwrap();
            let result = match path {
                0 => engine.sql(query, params),
                1 => engine.execute_prepared_statement(&prepared, params),
                _ => engine.sql(&format!("EXPLAIN (FORMAT JSON) {query}"), params),
            };
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("53200"),
                "{provider:?}, {mode}, path={path}: {error}"
            );
            drop(full);
            assert!(engine.transaction_failed());
            engine.sql("ROLLBACK TO planning_retry", &[]).unwrap();
            let result = match path {
                1 => engine
                    .execute_prepared_statement(&prepared, params)
                    .unwrap(),
                2 => engine
                    .sql(&format!("EXPLAIN (FORMAT JSON) {query}"), params)
                    .unwrap(),
                _ => engine.sql(query, params).unwrap(),
            };
            if path == 2 {
                let Some(Value::Json(text)) = result.value_at(0, 0) else {
                    panic!("JSON EXPLAIN after recovery");
                };
                let document: serde_json::Value = serde_json::from_str(text).unwrap();
                assert!(document[0]["Plan"].is_object());
            } else {
                assert_eq!(result.value_at(0, 0), Some(expected));
            }
            drop(result);
            engine.sql("ROLLBACK", &[]).unwrap();
        }
        engine.finalize_prepared_statement(&prepared);
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_scalar_translation_shares_request_resources(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("scalar-planning.db"));
    let mut input = UnifiedPlan::lower(
        uqa_sql::compile("SELECT substring($1 similar '%' escape '#')")
            .unwrap()
            .remove(0),
    );
    input.rewrite_scalar_expressions(&mut |expression| {
        *expression = uqa_sql::bind_type_introspection(
            std::mem::replace(expression, uqa_sql::ScalarExpr::Literal(Value::Null)),
            &uqa_sql::RowSchema::default(),
            &[],
        );
    });
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let expected =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input.clone())
            .unwrap();
    let before = memory.used();
    let full = memory.reserve(memory.limit() - before).unwrap();
    let error =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input.clone())
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    drop(full);
    assert_eq!(memory.used(), before);
    engine.cancel();
    let error =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input.clone())
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"), "{provider:?}: {error}");
    engine.reset_cancellation();
    let actual =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input).unwrap();
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    drop((actual, expected));
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_scalar_translation_requests_recover_after_rejection(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("scalar-requests.db");
    let engine = provider.open(&path);
    engine
        .sql(
            "CREATE TABLE words (word TEXT); INSERT INTO words VALUES ('Thomas')",
            &[],
        )
        .unwrap();
    let query = "SELECT substring(word similar '%' escape '#') FROM words WHERE word = $1";
    let expected = Value::Str("Thomas".into());
    let params = [uqa_sql::SQLParam::Scalar(expected.clone())];
    let retained = engine.query_retention_control().unwrap();
    verify_request_recovery(&engine, query, &params, &expected, provider);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(retained.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        let result = engine.sql(query, &params).unwrap();
        assert_eq!(result.value_at(0, 0), Some(&expected));
        drop(result);
        engine.close().unwrap();
    }
}
