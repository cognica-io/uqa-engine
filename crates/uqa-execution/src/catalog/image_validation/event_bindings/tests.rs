//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot, NoRoutines};
use uqa_core::{memory::RetainedValue, RelationIdentity};
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence, Statement, TableConstraintSet},
    catalog::{
        events::{RuleConditionBinding, RuleDependencies, StoredRule},
        roles::RoleIdentity,
        security::BoundTableSecurity,
        stored_view::{StoredView, StoredViewDefinition},
        view::StoredViewKind,
    },
    plan::{ExpressionPlan, UnifiedPlan},
    ColumnType,
};

fn catalog(owner: &str, condition: &str) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot(
            [1; 16],
            vec![
                ColumnDef::nullable("flag", ColumnType::Boolean),
                ColumnDef::nullable("id", ColumnType::Integer),
            ],
            TableConstraintSet::default(),
        ),
    );
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        uqa_sql::compile("SELECT flag,id FROM public.items")
            .unwrap()
            .remove(0),
    ) else {
        panic!("view query")
    };
    RetainedValue::make_mut(&mut snapshot.definitions.views)
        .insert(
            RelationIdentity::new("public", "visible"),
            StoredView {
                security: BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
                definition: StoredViewDefinition {
                    object_id: [2; 16],
                    query: *query,
                    output_columns: Some(vec!["flag".into(), "id".into()]),
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
    let Statement::CreateRule(definition) = uqa_sql::compile(&format!(
        "CREATE RULE saved AS ON UPDATE TO {owner} WHERE {condition} DO NOTHING"
    ))
    .unwrap()
    .remove(0) else {
        panic!("rule")
    };
    let condition_plan = Some(ExpressionPlan::lower(definition.condition.clone().unwrap()));
    let condition_binding = Some(RuleConditionBinding::for_event(
        &["flag".into(), "id".into()],
        definition.event,
    ));
    RetainedValue::make_mut(&mut snapshot.definitions.rules)
        .insert_event(
            RelationIdentity::from_legacy_name(owner).unwrap(),
            "saved".into(),
            StoredRule {
                definition,
                condition_plan,
                condition_binding,
                enabled: uqa_sql::ast::EventEnableMode::default(),
                dependencies: Some(RuleDependencies::default()),
                catalog_oid: Some(31_000),
            },
        )
        .unwrap();
    CatalogReadView::new(snapshot)
}

#[test]
fn native_event_condition_types_join_original_table_and_view_rows() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for owner in ["public.items", "public.visible"] {
        for (condition, valid) in [
            ("old.flag", true),
            ("old.id", false),
            ("EXISTS(SELECT 1 WHERE old.flag)", true),
        ] {
            let catalog = catalog(owner, condition);
            let result = validate(&catalog, &NoRoutines, &memory, &token);
            assert_eq!(result.is_ok(), valid, "{owner}: {condition}: {result:?}");
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn native_event_condition_types_retire_denied_and_cancelled_catalog_analysis() {
    let catalog = catalog("public.visible", "old.flag");
    for limit in [0, 32, 512] {
        let memory = MemoryBudget::new(limit);
        let error =
            validate(&catalog, &NoRoutines, &memory, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    token.cancel();
    assert!(validate(&catalog, &NoRoutines, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}
