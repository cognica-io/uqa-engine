//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{plan::CommandPlan, semantics::rules::binding::RuleMutation, ScalarExpr};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_mutation_rule_inputs_reject_before_payload_copies_and_recover(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mutation-rule-inputs.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer, payload text); INSERT INTO items VALUES(1,'kept'); CREATE TABLE log(event text, payload text); CREATE RULE capture_update AS ON UPDATE TO items DO ALSO INSERT INTO log VALUES('U', NEW.payload); CREATE RULE capture_delete AS ON DELETE TO items DO ALSO INSERT INTO log VALUES('D', OLD.payload)", &[]).unwrap();
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let before = memory.used();
    for sql in [
        "UPDATE public.items SET payload='changed' WHERE id=1",
        "DELETE FROM public.items WHERE payload='changed'",
    ] {
        let UnifiedPlan::Command(mut command) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("command")
        };
        let payload = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
        let mutation = match command.as_mut() {
            CommandPlan::Update(update) => {
                update.assignments[0].value = payload;
                RuleMutation::Update(update)
            }
            CommandPlan::Delete(delete) => {
                let Some(ScalarExpr::Binary { rhs, .. }) = delete.predicate.as_mut() else {
                    panic!("predicate")
                };
                **rhs = payload;
                RuleMutation::Delete(delete)
            }
            _ => panic!("mutation"),
        };
        let scope = crate::capabilities::query_scope::new_for_current_routine(&engine).unwrap();
        let context = engine.rule_execution_context();
        let baseline = memory.used();
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let observed = allocation_counter::measure(|| {
            let error = uqa_execution::mutation::rules::execute_mutation_query_rules(
                &context,
                mutation,
                &[],
                &scope,
            )
            .err()
            .expect("native input rejects a large owned payload");
            assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
        });
        assert!(
            observed.bytes_total < 256 << 10,
            "{provider:?}: {observed:?}"
        );
        assert_eq!(memory.used(), baseline + occupied.bytes());
        drop(occupied);
        drop(scope);
        assert_eq!(memory.used(), before);
    }
    assert!(engine
        .sql("SELECT * FROM log", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("SELECT payload FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str("kept".into()))
    );
    engine
        .sql("UPDATE items SET payload='changed'; DELETE FROM items", &[])
        .unwrap();
    let result = engine
        .sql("SELECT event, payload FROM log ORDER BY event", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    for (row, event) in ["D", "U"].into_iter().enumerate() {
        assert_eq!(result.value_at(row, 0), Some(&Value::Str(event.into())));
        assert_eq!(result.value_at(row, 1), Some(&Value::Str("changed".into())));
    }
    drop(result);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        assert_eq!(engine.sql("SELECT * FROM log", &[]).unwrap().rows.len(), 2);
        assert!(engine
            .sql("SELECT * FROM items", &[])
            .unwrap()
            .rows
            .is_empty());
        engine.close().unwrap();
    }
}
