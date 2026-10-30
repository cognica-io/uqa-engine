//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::Value;
use uqa_planner::UnifiedPlan;

const JOIN: &str = "SELECT a.id FROM a JOIN b ON a.id = b.id JOIN c ON b.id = c.id";

mod analysis_cache;
mod composite_constants;
mod demand;
mod indexes;
mod inference;
mod namespaces;
mod privileges;
mod retrieval;
mod retrieval_analysis;
mod routine_results;
mod rule_binding;
mod rule_mutations;
mod rule_schedules;
mod scalar_rewrite;
mod scopes;
mod sources;
mod statistics;
mod subqueries;
mod view_shapes;
mod view_targets;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_prepared_costing_shares_request_allowance_and_recovers(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("costing-owner.db"));
    engine.sql("CREATE TABLE items (id integer PRIMARY KEY, payload text); INSERT INTO items VALUES (1, 'x')", &[]).unwrap();
    let mut plan = UnifiedPlan::lower(
        uqa_sql::compile("SELECT id FROM items WHERE id = $1 AND payload = 'x'::text")
            .unwrap()
            .remove(0),
    );
    let UnifiedPlan::Query(query) = &mut plan else {
        panic!("query")
    };
    let uqa_planner::RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let uqa_sql::ScalarExpr::And(predicates) = block.r#where.as_mut().unwrap() else {
        panic!("conjunction")
    };
    let uqa_sql::ScalarExpr::Binary { rhs, .. } = &mut predicates[1] else {
        panic!("comparison")
    };
    let uqa_sql::ScalarExpr::Cast { expr, .. } = rhs.as_mut() else {
        panic!("cast")
    };
    **expr = uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let estimate = || crate::capabilities::statement_planning::estimate_engine_plan(&engine, &plan);
    let expected = estimate().unwrap();
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 16384).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| result = Some(estimate()));
    assert_eq!(
        result.unwrap().unwrap_err().sqlstate(),
        Some("53200"),
        "{provider:?}"
    );
    assert!(
        allocation.bytes_total < 256 * 1024,
        "{provider:?}: {allocation:?}"
    );
    drop(occupied);
    assert_eq!(memory.used(), before);
    engine.cancel();
    assert_eq!(
        estimate().unwrap_err().sqlstate(),
        Some("57014"),
        "{provider:?}"
    );
    engine.reset_cancellation();
    let actual = estimate().unwrap();
    assert_eq!(actual.rows.to_bits(), expected.rows.to_bits());
    assert_eq!(actual.execution.to_bits(), expected.execution.to_bits());
    assert_eq!(actual.startup.to_bits(), expected.startup.to_bits());
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_constant_production_recovers_without_repeating_effects(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constant-production.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE constant_effects (n integer, tick bigint); CREATE SEQUENCE constant_ticks", &[]).unwrap();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let mut effects = 0_i64;
    for mode in ["force_custom_plan", "force_generic_plan"] {
        engine
            .sql(&format!("SET plan_cache_mode = {mode}"), &[])
            .unwrap();
        for path in 0..3 {
            let query = "INSERT INTO constant_effects SELECT length(repeat('x', 4194304)), nextval('constant_ticks') RETURNING n";
            let prepared = engine.prepare_statement(query, &[]).unwrap();
            engine.sql("BEGIN; SAVEPOINT retry_constant", &[]).unwrap();
            let occupied = memory.reserve(memory.available() - 32768).unwrap();
            let error = match path {
                0 => engine.sql(query, &[]),
                1 => engine.execute_prepared_statement(&prepared, &[]),
                _ => engine.sql(
                    &format!("EXPLAIN (MEMORY, ANALYZE, FORMAT JSON) {query}"),
                    &[],
                ),
            }
            .unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("53200"),
                "{provider:?}, {mode}, path={path}: {error}"
            );
            drop(occupied);
            assert!(engine.transaction_failed());
            engine.sql("ROLLBACK TO retry_constant", &[]).unwrap();
            assert_eq!(
                engine
                    .sql("SELECT count(*) FROM constant_effects", &[])
                    .unwrap()
                    .value_at(0, 0),
                Some(&Value::Int(effects))
            );
            let result = match path {
                0 => engine.sql(query, &[]),
                1 => engine.execute_prepared_statement(&prepared, &[]),
                _ => engine.sql(
                    &format!("EXPLAIN (MEMORY, ANALYZE, FORMAT JSON) {query}"),
                    &[],
                ),
            }
            .unwrap();
            if path != 2 {
                assert_eq!(result.value_at(0, 0), Some(&Value::Int(4_194_304)));
            }
            drop(result);
            effects += 1;
            assert_eq!(
                engine
                    .sql("SELECT currval('constant_ticks')", &[])
                    .unwrap()
                    .value_at(0, 0),
                Some(&Value::Int(effects))
            );
            engine.sql("COMMIT", &[]).unwrap();
            engine.finalize_prepared_statement(&prepared);
        }
    }
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM constant_effects", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(6))
    );
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        assert_eq!(
            engine
                .sql(
                    "SELECT count(*), min(n), max(tick) FROM constant_effects",
                    &[]
                )
                .unwrap()
                .value_at(0, 2),
            Some(&Value::Int(6))
        );
        engine.close().unwrap();
    }
}

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
    assert_eq!(
        format!("{:?}", actual.as_ref()),
        format!("{:?}", expected.as_ref())
    );
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
    let joins = [
        JOIN,
        "SELECT a.id FROM a JOIN b USING (id) JOIN c USING (id)",
        "SELECT a.id FROM a NATURAL JOIN b NATURAL JOIN c",
    ];
    let params = [uqa_sql::SQLParam::Scalar(Value::Int(7))];
    for join in joins {
        let query = format!("{join} WHERE a.id = $1 AND a.id >= 0");
        verify_request_recovery(&engine, &query, &params, &Value::Int(7), provider);
    }
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        for join in joins {
            let result = reopened.sql(join, &[]).unwrap();
            assert_eq!(result.value_at(0, 0), Some(&Value::Int(7)));
        }
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
    assert_eq!(
        format!("{:?}", actual.as_ref()),
        format!("{:?}", expected.as_ref())
    );
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

mod rule_references;
mod rule_selection;
mod rule_targets;
