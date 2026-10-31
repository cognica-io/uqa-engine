//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::sources::build_join_operator_with_ctes;
use uqa_sql::plan::RelationalPlan;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_view_sources_admit_selected_queries_before_copying_stored_definitions(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("view-source-owners.db"));
    let payload = "x".repeat(4 << 20);
    for (name, invoker) in [("owned_view", false), ("invoking_view", true)] {
        engine.sql(&format!("CREATE VIEW {name} WITH(security_invoker={invoker}) AS SELECT '{payload}'::text AS payload"), &[]).unwrap();
    }
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let context = engine.source_execution_context();
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    let original = scope.privilege_subject().unwrap().clone();
    for name in ["owned_view", "invoking_view"] {
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(
            uqa_sql::compile(&format!("SELECT payload FROM {name}"))
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block")
        };
        let source = block.from.as_ref().unwrap();
        let baseline = memory.used();
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(build_join_operator_with_ctes(
                &context,
                source,
                &[],
                &mut scope,
                None,
                None,
            ));
        });
        assert!(
            allocation.bytes_total < 256 << 10,
            "{provider:?} {name}: {allocation:?}"
        );
        assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
        assert_eq!(scope.privilege_subject().unwrap(), &original);
        assert_eq!(memory.used(), baseline + occupied.bytes());
        drop(occupied);
        for token in [
            resources.cancellation(),
            engine.query_runtime_view().cancellation,
        ] {
            token.cancel();
            assert_eq!(
                build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None)
                    .err()
                    .unwrap()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(scope.privilege_subject().unwrap(), &original);
            assert_eq!(memory.used(), baseline);
            token.reset();
        }
        let mut operator =
            build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None).unwrap();
        assert_eq!(operator.schema().len(), 1);
        operator.open().unwrap();
        let batch = operator.next().unwrap().unwrap();
        assert_eq!(batch.rows.len(), 1);
        let rows = batch.into_result_rows();
        assert!(matches!(rows[0].values().next(), Some(Value::Str(value)) if value == &payload));
        operator.close().unwrap();
        drop((rows, operator));
        assert_eq!(scope.privilege_subject().unwrap(), &original);
        assert_eq!(memory.used(), baseline);
    }
    drop((scope, original));
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_view_authority_preserves_invoker_checks_preparation_and_rollback(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("view-authority-behavior.db");
    let engine = provider.open(&path);
    engine.sql("CREATE ROLE authority_owner NOLOGIN; CREATE ROLE authority_reader NOLOGIN; CREATE SCHEMA authority_reference; GRANT USAGE,CREATE ON SCHEMA authority_reference TO authority_owner; GRANT USAGE ON SCHEMA authority_reference TO authority_reader; SET ROLE authority_owner; CREATE TABLE authority_reference.items(id integer); INSERT INTO authority_reference.items VALUES(7); CREATE VIEW authority_reference.defining AS SELECT id,current_user::text AS actor FROM authority_reference.items; CREATE VIEW authority_reference.invoking WITH(security_invoker=true) AS SELECT id,current_user::text AS actor FROM authority_reference.items; CREATE VIEW authority_reference.nested AS SELECT id,actor FROM authority_reference.invoking; GRANT SELECT ON authority_reference.defining,authority_reference.invoking,authority_reference.nested TO authority_reader; GRANT UPDATE(id) ON authority_reference.invoking,authority_reference.nested TO authority_reader; RESET ROLE; SET ROLE authority_reader; SET search_path=authority_reference", &[]).unwrap();
    let check = |sql: &str, id: i64| {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), 1, "{provider:?}: {sql}");
        assert_eq!(
            result.value_at(0, 0),
            Some(&Value::Int(id)),
            "{provider:?}: {sql}"
        );
        assert_eq!(
            result.value_at(0, 1),
            Some(&Value::Str("authority_reader".into())),
            "{provider:?}: {sql}"
        );
    };
    check("SELECT id,actor FROM defining", 7);
    engine.sql("BEGIN; SAVEPOINT before_denial", &[]).unwrap();
    for view in ["nested", "invoking"] {
        assert_eq!(
            engine
                .sql(&format!("SELECT id,actor FROM {view}"), &[])
                .unwrap_err()
                .sqlstate(),
            Some("42501")
        );
        assert_eq!(
            engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
            Some("25P02")
        );
        engine.sql("ROLLBACK TO before_denial", &[]).unwrap();
        assert_eq!(
            engine
                .sql(&format!("UPDATE {view} SET id=9 RETURNING id,actor"), &[])
                .unwrap_err()
                .sqlstate(),
            Some("42501")
        );
        engine.sql("ROLLBACK TO before_denial", &[]).unwrap();
    }
    engine.sql("COMMIT; RESET ROLE; GRANT SELECT,UPDATE(id) ON authority_reference.items TO authority_reader; GRANT UPDATE(id) ON authority_reference.invoking TO authority_reader; SET ROLE authority_reader; PREPARE authority_saved AS SELECT id,actor FROM invoking", &[]).unwrap();
    for mode in ["force_generic_plan", "force_custom_plan"] {
        engine
            .sql(&format!("SET plan_cache_mode={mode}"), &[])
            .unwrap();
        check("EXECUTE authority_saved", 7);
    }
    engine
        .sql(
            "BEGIN; DECLARE authority_cursor CURSOR FOR SELECT id,actor FROM invoking",
            &[],
        )
        .unwrap();
    check("FETCH ALL FROM authority_cursor", 7);
    engine
        .sql("CLOSE authority_cursor; SAVEPOINT before_update", &[])
        .unwrap();
    check("UPDATE invoking SET id=8 RETURNING id,actor", 8);
    check("UPDATE nested SET id=9 RETURNING id,actor", 9);
    engine
        .sql("ROLLBACK TO before_update; COMMIT", &[])
        .unwrap();
    check("SELECT id,actor FROM invoking", 7);
    check("SELECT id,actor FROM nested", 7);
    engine.sql("EXPLAIN(MEMORY,FORMAT JSON) SELECT id,actor FROM invoking; EXPLAIN(MEMORY,FORMAT JSON) UPDATE invoking SET id=9 RETURNING id,actor; DEALLOCATE authority_saved", &[]).unwrap();
    check("SELECT id,actor FROM invoking", 7);
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        let result = reopened.sql("SET ROLE authority_reader; SET search_path=authority_reference; SELECT id,actor FROM nested", &[]).unwrap();
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(7)));
        assert_eq!(
            result.value_at(0, 1),
            Some(&Value::Str("authority_reader".into()))
        );
        reopened.close().unwrap();
    }
}
