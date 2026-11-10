//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_graph::GraphStore;
use uqa_sql::catalog::events::{RuleColumnDependency, RuleDependencies, StoredRule};

fn snapshot() -> crate::catalog::CatalogReadSnapshot {
    let mut snapshot = catalog("SELECT id FROM public.items").snapshot().clone();
    snapshot.definitions.foreign_tables = crate::catalog::test_support::foreign_tables([(
        RelationIdentity::new("public", "remote"),
        crate::catalog::foreign::StoredForeignTable {
            name: "public.remote".into(),
            persistence: RelationPersistence::Permanent,
            object_id: [3; 16],
            catalog_oids: None,
            row_type_array_name: None,
            server_name: "remote_server".into(),
            server_reference: None,
            columns: vec![ColumnDef::nullable("id", ColumnType::Integer)],
            dropped_attributes: vec![],
            checks: vec![],
            options: uqa_core::memory::OwnedMap::default(),
            option_order: vec![],
        },
    )]);
    let mut graph = uqa_graph::GraphStoreHandle::default();
    graph.create_graph("native_graph").unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.sequences)
        .insert(
            RelationIdentity::new("public", "ticket"),
            crate::catalog::sequence::registry::SequenceRecord {
                state: crate::catalog::sequence::SequenceState::initial(
                    1,
                    1,
                    uqa_sql::ast::SequenceDataType::BigInt,
                ),
                object_id: [4; 16],
                persistence: RelationPersistence::Permanent,
                catalog_oid: None,
            },
            uqa_sql::catalog::security::BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.graphs)
        .insert("native_graph", std::sync::Arc::new(graph))
        .unwrap();
    snapshot
}

fn rule() -> StoredRule {
    let Statement::CreateRule(definition) =
        uqa_sql::compile("CREATE RULE saved AS ON INSERT TO public.items DO NOTHING")
            .unwrap()
            .remove(0)
    else {
        panic!("rule");
    };
    StoredRule {
        definition,
        enabled: uqa_sql::ast::EventEnableMode::default(),
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
        catalog_oid: Some(31_000),
    }
}

#[test]
fn native_event_dependencies_use_captured_table_view_foreign_system_and_graph_columns() {
    for (name, column) in [
        ("public.items", "id"),
        ("public.visible", "id"),
        ("public.remote", "id"),
        ("pg_catalog.pg_class", "oid"),
        ("public.ticket", "last_value"),
        ("public.ticket", "log_cnt"),
        ("public.ticket", "is_called"),
        ("native_graph._ag_label_vertex", "id"),
        ("native_graph._ag_label_edge", "start_id"),
    ] {
        for missing in [false, true] {
            let mut snapshot = snapshot();
            let mut rule = rule();
            let mut dependencies = RuleDependencies::default();
            dependencies.columns.insert(RuleColumnDependency {
                relation: RelationIdentity::from_legacy_name(name).unwrap(),
                column: if missing { "absent" } else { column }.into(),
            });
            rule.dependencies = Some(dependencies);
            RetainedValue::make_mut(&mut snapshot.definitions.rules)
                .insert_event(
                    RelationIdentity::new("public", "items"),
                    "saved".into(),
                    rule,
                )
                .unwrap();
            let result = check(&CatalogReadView::new(snapshot));
            assert_eq!(
                result.is_ok(),
                !missing,
                "{name}.{column}, missing={missing}: {result:?}"
            );
        }
    }
}

#[test]
fn native_event_dependencies_keep_document_fields_open_and_require_live_event_owners() {
    for declared in [false, true] {
        let mut snapshot = snapshot();
        snapshot
            .tables
            .get_mut(&RelationIdentity::new("public", "items"))
            .unwrap()
            .columns_declared = declared;
        let mut rule = rule();
        let mut dependencies = RuleDependencies::default();
        dependencies.columns.insert(RuleColumnDependency {
            relation: RelationIdentity::new("public", "items"),
            column: "dynamic_field".into(),
        });
        rule.dependencies = Some(dependencies);
        RetainedValue::make_mut(&mut snapshot.definitions.rules)
            .insert_event(
                RelationIdentity::new("public", "items"),
                "saved".into(),
                rule,
            )
            .unwrap();
        assert_eq!(check(&CatalogReadView::new(snapshot)).is_ok(), !declared);
    }
    let mut snapshot = snapshot();
    let mut rule = rule();
    rule.definition.table = "public.absent".into();
    RetainedValue::make_mut(&mut snapshot.definitions.rules)
        .insert_event(
            RelationIdentity::new("public", "absent"),
            "saved".into(),
            rule,
        )
        .unwrap();
    assert!(check(&CatalogReadView::new(snapshot)).is_err());
}
