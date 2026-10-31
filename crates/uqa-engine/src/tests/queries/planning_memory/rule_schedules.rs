//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    plan::{CommandPlan, RelationalPlan, ViewRuleInsertPlan},
    semantics::rules::binding::OwnedInsertRuleQueries,
    ScalarExpr,
};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_insert_rule_schedules_reject_before_copies_and_recover(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("insert-rule-schedules.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer, payload text DEFAULT 'base'); CREATE TABLE log(id integer, payload text); CREATE VIEW v1 AS SELECT * FROM items; CREATE VIEW v2 AS SELECT * FROM v1; CREATE RULE skip_negative AS ON INSERT TO items WHERE NEW.id < 0 DO INSTEAD NOTHING; CREATE RULE capture_insert AS ON INSERT TO items DO ALSO INSERT INTO log VALUES(NEW.id, NEW.payload)", &[]).unwrap();
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let context = engine.rule_execution_context();
    let control = ProductionControl::new(memory, retained.cancellation(), context.cancellation);
    let subject = context.security.privilege_subject("public.items").unwrap();
    let before = memory.used();
    for sql in [
        "INSERT INTO public.items(id,payload) VALUES(8,'payload')",
        "INSERT INTO public.items(id,payload) SELECT 8,'payload'",
    ] {
        let UnifiedPlan::Command(command) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("command")
        };
        let CommandPlan::Insert(mut insert) = *command else {
            panic!("INSERT")
        };
        let payload = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
        if let Some(source) = &mut insert.source {
            let RelationalPlan::QueryBlock(block) = &mut source.root else {
                panic!("query")
            };
            block.projections[1].expr = payload;
        } else {
            insert.rows[0][1] = payload;
        }
        insert.view_rule_insert_plans = ["public.v2", "public.v1"]
            .into_iter()
            .map(|relation| ViewRuleInsertPlan {
                relation: relation.into(),
                supplied_columns: vec!["id".into(), "payload".into()],
                input_columns: vec!["id".into(), "payload".into()],
            })
            .collect();
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let observed = allocation_counter::measure(|| {
            let error = OwnedInsertRuleQueries::bind(
                context.analysis,
                &insert,
                &|relation| context.security.privilege_subject(relation),
                &subject,
                &control,
            )
            .err()
            .expect("native INSERT source must reject before copying");
            assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
        });
        assert!(
            observed.bytes_total < 256 << 10,
            "{provider:?}: {observed:?}"
        );
        assert_eq!(memory.used(), before + occupied.bytes());
        drop(occupied);
        assert_eq!(memory.used(), before);
    }
    assert!(engine
        .sql("SELECT * FROM items", &[])
        .unwrap()
        .rows
        .is_empty());
    assert!(engine
        .sql("SELECT * FROM log", &[])
        .unwrap()
        .rows
        .is_empty());
    engine.sql("INSERT INTO v2(id) VALUES(1),(2); INSERT INTO v2 SELECT 3,'query'; INSERT INTO v2 VALUES(-1,'suppressed')", &[]).unwrap();
    assert_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_rows(&reopened);
        reopened.close().unwrap();
    }
}

fn assert_rows(engine: &Engine) {
    let rows = engine
        .sql("SELECT id,payload FROM items ORDER BY id", &[])
        .unwrap();
    assert_eq!(rows.rows.len(), 3);
    for (index, payload) in ["base", "base", "query"].into_iter().enumerate() {
        assert_eq!(
            rows.value_at(index, 0),
            Some(&Value::Int(i64::try_from(index).unwrap() + 1))
        );
        assert_eq!(rows.value_at(index, 1), Some(&Value::Str(payload.into())));
    }
    let log = engine
        .sql("SELECT id,payload FROM log ORDER BY id", &[])
        .unwrap();
    assert_eq!(log.rows.len(), 4);
    assert_eq!(log.value_at(0, 0), Some(&Value::Int(-1)));
    assert_eq!(log.value_at(0, 1), Some(&Value::Str("suppressed".into())));
}
