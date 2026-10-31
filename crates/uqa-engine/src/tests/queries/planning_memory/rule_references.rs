//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, CancellationToken};
use uqa_sql::{
    ast::{Expr, Statement},
    catalog::events::StoredRule,
    semantics::view_rewrite::context::ViewRewriteCatalog,
};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_rule_reference_inspection_borrows_payloads_and_admits_names(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("rule-reference.db"));
    let Statement::CreateRule(definition) =
        uqa_sql::compile("CREATE RULE r AS ON UPDATE TO t DO ALSO SELECT new.n, 'payload'")
            .unwrap()
            .remove(0)
    else {
        panic!("rule")
    };
    let mut rule = StoredRule {
        definition,
        enabled: uqa_sql::ast::EventEnableMode::default(),
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
        catalog_oid: None,
    };
    let Statement::Select(query) = &mut rule.definition.actions[0] else {
        panic!("query")
    };
    query.projections[1].expr = Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, resources.cancellation(), &invoking);
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 16 * 1024).unwrap();
    let measured = allocation_counter::measure(|| {
        let columns =
            ViewRewriteCatalog::rule_new_row_columns_with_control(&engine, &rule, &control)
                .unwrap()
                .unwrap();
        assert_eq!(&*columns, &["n"]);
    });
    assert!(
        measured.bytes_total < 16 * 1024,
        "{provider:?}: {measured:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    let Statement::Select(query) = &mut rule.definition.actions[0] else {
        panic!("query")
    };
    query.projections[0].expr = Expr::QualifiedColumn {
        qualifier: "new".into(),
        column: "n".repeat(4 * 1024 * 1024).into(),
    };
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            ViewRewriteCatalog::rule_new_row_columns_with_control(&engine, &rule, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(
        measured.bytes_total < 16 * 1024,
        "{provider:?}: {measured:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    for token in [resources.cancellation(), &invoking] {
        token.cancel();
        assert_eq!(
            ViewRewriteCatalog::rule_new_row_columns_with_control(&engine, &rule, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), before);
        token.reset();
    }
    drop(ViewRewriteCatalog::rule_new_row_columns_with_control(&engine, &rule, &control).unwrap());
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
