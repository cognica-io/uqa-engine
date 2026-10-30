//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, CancellationToken, RelationIdentity};
use uqa_sql::{
    ast::Expr, plan::RelationalPlan, semantics::rules::action_binding::RuleSourceCatalog,
    ScalarExpr,
};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_rule_target_columns_borrow_declarations_and_recover(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("rule-target.db"));
    engine.sql("CREATE TABLE items (n text DEFAULT 'original'); CREATE VIEW selected AS SELECT 'original'::text AS n; CREATE MATERIALIZED VIEW materialized AS SELECT 'original'::text AS n", &[]).unwrap();
    let table = engine.try_table("items").unwrap().unwrap();
    table.columns.write()[0].default = Some(Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024))));
    {
        let mut views = engine.durable.views.write();
        let view = views
            .get_mut(&RelationIdentity::new("public", "selected"))
            .unwrap();
        let RelationalPlan::QueryBlock(block) = &mut view.query.root else {
            panic!("query")
        };
        block.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
        let materialized = views
            .get_mut(&RelationIdentity::new("public", "materialized"))
            .unwrap();
        materialized.materialized_rows[0]
            .insert("n".into(), Value::Str("x".repeat(4 * 1024 * 1024)));
    }
    for target in ["items", "selected", "materialized"] {
        let copied = allocation_counter::measure(|| {
            drop(RuleSourceCatalog::rule_relation_definitions(&engine, target).unwrap());
        });
        assert!(
            copied.bytes_total >= 4 * 1024 * 1024,
            "{provider:?}: {target}: {copied:?}"
        );
    }
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, resources.cancellation(), &invoking);
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 64 * 1024).unwrap();
    for target in ["items", "materialized"] {
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = Some(
                RuleSourceCatalog::rule_relation_columns_with_control(&engine, target, &control)
                    .unwrap(),
            );
        });
        let output = output.unwrap();
        assert_eq!(&*output, &["n"], "{provider:?}: {target}");
        assert!(
            measured.bytes_total < 64 * 1024,
            "{provider:?}: {target}: {measured:?}"
        );
        assert_eq!(
            memory.used(),
            before + occupied.bytes() + output.reserved_bytes()
        );
        drop(output);
        assert_eq!(memory.used(), before + occupied.bytes());
    }
    table.columns.write()[0].name = "x".repeat(4 * 1024 * 1024);
    // The view still requires an admitted analysis tree to validate its row schema; table defaults and materialized rows are irrelevant to that descriptor.
    for target in ["items", "selected"] {
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                RuleSourceCatalog::rule_relation_columns_with_control(&engine, target, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(
            measured.bytes_total < 64 * 1024,
            "{provider:?}: {target}: {measured:?}"
        );
        assert_eq!(memory.used(), before + occupied.bytes());
    }
    table.columns.write()[0].name = "n".into();
    drop(occupied);
    for token in [resources.cancellation(), &invoking] {
        token.cancel();
        for target in ["items", "selected", "materialized"] {
            assert_eq!(
                RuleSourceCatalog::rule_relation_columns_with_control(&engine, target, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        }
        assert_eq!(memory.used(), before);
        token.reset();
    }
    for target in ["items", "selected", "materialized"] {
        assert_eq!(
            &*RuleSourceCatalog::rule_relation_columns_with_control(&engine, target, &control)
                .unwrap(),
            &["n"]
        );
    }
    assert_eq!(memory.used(), before);
    drop(table);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_rule_target_columns_preserve_live_lookup_and_schema_diagnostics(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("rule-target-lookup.db"));
    engine.sql("CREATE SCHEMA elsewhere; CREATE TABLE elsewhere.shared (table_column integer); CREATE VIEW public.shared AS SELECT 1 AS view_column; CREATE VIEW elsewhere.other AS SELECT 2 AS other_column; CREATE VIEW public.other AS SELECT 3 AS public_column; CREATE TABLE public.\"a.b\" (\"Mixed.Name\" integer); CREATE MATERIALIZED VIEW public.materialized AS SELECT 'x'::text AS label; SET search_path TO elsewhere, public", &[]).unwrap();
    let resources = engine.query_retention_control().unwrap();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(resources.memory(), resources.cancellation(), &invoking);
    let before = resources.memory().used();
    for (name, expected) in [
        ("shared", "table_column"),
        ("public.shared", "view_column"),
        ("other", "public_column"),
        ("elsewhere.other", "other_column"),
        ("public.\"a.b\"", "Mixed.Name"),
        ("public.materialized", "label"),
    ] {
        let ordinary = RuleSourceCatalog::rule_relation_definitions(&engine, name).unwrap();
        let names =
            RuleSourceCatalog::rule_relation_columns_with_control(&engine, name, &control).unwrap();
        assert_eq!(&*names, &[expected], "{provider:?}: {name}");
        assert_eq!(
            names.iter().collect::<Vec<_>>(),
            ordinary
                .iter()
                .map(|column| &column.name)
                .collect::<Vec<_>>()
        );
    }
    for name in ["missing", "missing_schema.item", "\"unterminated"] {
        let ordinary = RuleSourceCatalog::rule_relation_definitions(&engine, name).unwrap_err();
        let error = RuleSourceCatalog::rule_relation_columns_with_control(&engine, name, &control)
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            ordinary.sqlstate(),
            "{provider:?}: {name}"
        );
        assert_eq!(
            error.to_string(),
            ordinary.to_string(),
            "{provider:?}: {name}"
        );
    }
    {
        let mut views = engine.durable.views.write();
        let materialized = views
            .get_mut(&RelationIdentity::new("public", "materialized"))
            .unwrap();
        materialized.materialized_column_types.clear();
    }
    let ordinary =
        RuleSourceCatalog::rule_relation_definitions(&engine, "public.materialized").unwrap_err();
    let error = RuleSourceCatalog::rule_relation_columns_with_control(
        &engine,
        "public.materialized",
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), ordinary.sqlstate());
    assert_eq!(error.to_string(), ordinary.to_string());
    assert_eq!(resources.memory().used(), before);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(resources.memory().used(), 0);
}
