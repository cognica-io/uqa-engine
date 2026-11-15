//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{test_support::NoRoutines, CatalogReadView};
use uqa_sql::ast::Expr;

fn with_check(kind: &str, expression: Expr) -> CatalogReadView {
    let mut snapshot = constraint_catalog(10, 11, 12);
    match kind {
        "ordinary" => {
            let table = snapshot
                .tables
                .get_mut(&RelationIdentity::new("public", "ordinary"))
                .unwrap();
            let mut columns = table.columns.to_vec();
            columns[0].check = Some(expression);
            table.columns = columns.into();
        }
        "foreign" => {
            let name = RelationIdentity::new("public", "remote");
            let mut table = snapshot
                .definitions
                .foreign_tables
                .get(&name)
                .unwrap()
                .clone();
            table.columns[0].check = Some(expression);
            snapshot.definitions.foreign_tables =
                crate::catalog::test_support::foreign_tables([(name, table)]);
        }
        "domain" => {
            let mut domain = snapshot
                .definitions
                .domains
                .get("public.positive")
                .unwrap()
                .clone();
            domain.definition.checks.push(uqa_sql::ast::DomainCheck {
                name: Some("positive".into()),
                catalog_identity: Some(identity(20)),
                expression,
                validated: false,
            });
            RetainedValue::make_mut(&mut snapshot.definitions.domains)
                .insert("public.positive".into(), domain)
                .unwrap();
        }
        _ => unreachable!(),
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn native_constraint_bindings_join_original_ordinary_foreign_and_domain_scopes() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for kind in ["ordinary", "foreign", "domain"] {
        let valid = with_check(kind, Expr::Literal(uqa_core::Value::Bool(true)));
        validate_constraint_bindings(&valid, &NoRoutines, &memory, &token).unwrap();
        assert_eq!(memory.used(), 0);
        let name = if kind == "domain" { "value" } else { "id" };
        let invalid = with_check(kind, Expr::Column(name.into()));
        let error =
            validate_constraint_bindings(&invalid, &NoRoutines, &memory, &token).unwrap_err();
        assert!(error.to_string().contains("non-boolean"), "{kind}: {error}");
        assert_eq!(memory.used(), 0);
        if kind == "domain" {
            assert!(
                !invalid
                    .snapshot()
                    .definitions
                    .domains
                    .get("public.positive")
                    .unwrap()
                    .definition
                    .checks[0]
                    .validated
            );
        }
    }
}

#[test]
fn native_constraint_bindings_keep_table_and_domain_namespaces_distinct() {
    let memory = MemoryBudget::new(1 << 20);
    for (kind, column) in [
        ("ordinary", "value"),
        ("foreign", "value"),
        ("domain", "id"),
        ("domain", "tableoid"),
    ] {
        let catalog = with_check(kind, Expr::Column(column.into()));
        let error =
            validate_constraint_bindings(&catalog, &NoRoutines, &memory, &CancellationToken::new())
                .unwrap_err();
        assert!(error.to_string().contains(column), "{kind}: {error}");
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_constraint_bindings_release_catalog_workspace_on_denial_and_cancellation() {
    let catalog = with_check("domain", Expr::Literal(uqa_core::Value::Null));
    let token = CancellationToken::new();
    let denied = MemoryBudget::new(0);
    let error = validate_constraint_bindings(&catalog, &NoRoutines, &denied, &token).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(denied.used(), 0);
    let memory = MemoryBudget::new(1 << 20);
    token.cancel();
    assert!(validate_constraint_bindings(&catalog, &NoRoutines, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    validate_constraint_bindings(&catalog, &NoRoutines, &memory, &CancellationToken::new())
        .unwrap();
    assert_eq!(memory.used(), 0);
}
