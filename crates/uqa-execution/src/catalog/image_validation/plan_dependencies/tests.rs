//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot};
use uqa_core::{
    memory::{MemoryBudget, RetainedValue},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    ast::{ColumnDef, FunctionBody, RelationPersistence, Statement, TableConstraintSet},
    catalog::{
        roles::RoleIdentity,
        security::BoundTableSecurity,
        stored_view::{StoredView, StoredViewDefinition},
        view::StoredViewKind,
    },
    plan::UnifiedPlan,
    routines::SQLUserFunction,
    ColumnType,
};

mod events;

fn catalog(sql: &str) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot(
            [1; 16],
            vec![ColumnDef::nullable("id", ColumnType::Integer)],
            TableConstraintSet::default(),
        ),
    );
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    RetainedValue::make_mut(&mut snapshot.definitions.views)
        .insert(
            RelationIdentity::new("public", "visible"),
            StoredView {
                security: BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
                definition: StoredViewDefinition {
                    object_id: [2; 16],
                    query: *query,
                    output_columns: Some(vec!["id".into()]),
                    persistence: RelationPersistence::Permanent,
                    options: vec![],
                    kind: StoredViewKind::View,
                    materialized_rows: vec![],
                    materialized_column_types: vec![],
                    materialized_column_collations: None,
                    populated: true,
                    catalog_oids: None,
                    row_type_array_name: None,
                },
            },
        )
        .unwrap();
    CatalogReadView::new(snapshot)
}

fn check(catalog: &CatalogReadView) -> Result<(), SQLError> {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let result = validate(catalog, &ProductionControl::new(&memory, &token, &token));
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_stored_relations_reject_missing_rule_action_targets() {
    let mut snapshot = catalog("SELECT id FROM public.items").snapshot().clone();
    let Statement::CreateRule(definition) = uqa_sql::compile(
        "CREATE RULE saved AS ON INSERT TO public.items DO ALSO INSERT INTO public.missing VALUES (new.id)",
    )
    .unwrap()
    .remove(0) else {
        panic!("rule");
    };
    RetainedValue::make_mut(&mut snapshot.definitions.rules)
        .insert_event(
            RelationIdentity::new("public", "items"),
            "saved".into(),
            uqa_sql::catalog::events::StoredRule {
                definition,
                enabled: uqa_sql::ast::EventEnableMode::default(),
                condition_plan: None,
                condition_binding: None,
                dependencies: Some(uqa_sql::catalog::events::RuleDependencies::default()),
                catalog_oid: Some(31_000),
            },
        )
        .unwrap();
    let result = check(&CatalogReadView::new(snapshot));
    assert!(
        result.is_err(),
        "missing stored rule target accepted: {result:?}"
    );
}

#[test]
fn native_stored_relations_accept_ctes_and_reject_missing_or_unqualified_sources() {
    check(&catalog(
        "WITH items AS (SELECT id FROM public.items) SELECT * FROM items",
    ))
    .unwrap();
    check(&catalog("SELECT oid AS id FROM pg_catalog.pg_class")).unwrap();
    for sql in [
        "SELECT * FROM public.missing",
        "SELECT * FROM items",
        "WITH items AS (SELECT * FROM items) SELECT * FROM items",
    ] {
        assert!(check(&catalog(sql)).is_err(), "{sql}");
    }
}

#[test]
fn native_stored_relations_validate_bound_routines_but_keep_source_bodies_lazy() {
    for (sql, valid) in [
        (
            "CREATE FUNCTION public.f() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT id FROM public.items; END",
            true,
        ),
        (
            "CREATE FUNCTION public.f() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT id FROM public.missing; END",
            false,
        ),
        (
            "CREATE FUNCTION public.f() RETURNS integer LANGUAGE SQL AS 'SELECT id FROM public.missing'",
            true,
        ),
    ] {
        let mut snapshot = catalog("SELECT id FROM public.items").snapshot().clone();
        let Statement::CreateFunction(definition) = uqa_sql::compile(sql).unwrap().remove(0) else {
            panic!("function");
        };
        let body = match &definition.body {
            FunctionBody::Source(_) => RoutineBody::Source,
            FunctionBody::Statements(statements) => RoutineBody::Bound(
                CompiledFunctionBody::SQL(
                    statements.iter().cloned().map(UnifiedPlan::lower).collect(),
                )
                .shared(),
            ),
        };
        RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
            .insert(
                "public.f".into(),
                vec![SQLUserFunction::new(*definition, body).shared()],
            )
            .unwrap();
        assert_eq!(
            check(&CatalogReadView::new(snapshot)).is_ok(),
            valid,
            "{sql}"
        );
    }
}

#[test]
fn native_stored_relations_preserve_catalog_and_release_denied_workspace() {
    let catalog = catalog("WITH items AS (SELECT * FROM public.items) SELECT * FROM items");
    let before = serde_json::to_value(
        catalog
            .snapshot()
            .definitions
            .views
            .iter()
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let token = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    assert!(validate(&catalog, &ProductionControl::new(&empty, &token, &token)).is_err());
    assert_eq!(empty.used(), 0);
    let memory = MemoryBudget::new(64 << 10);
    token.cancel();
    assert!(validate(&catalog, &ProductionControl::new(&memory, &token, &token)).is_err());
    assert_eq!(memory.used(), 0);
    check(&catalog).unwrap();
    assert_eq!(
        serde_json::to_value(
            catalog
                .snapshot()
                .definitions
                .views
                .iter()
                .collect::<Vec<_>>()
        )
        .unwrap(),
        before
    );
}
