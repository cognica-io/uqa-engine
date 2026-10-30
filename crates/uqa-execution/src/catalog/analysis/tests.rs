//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    foreign::StoredForeignTable, test_support::empty_catalog, RelationLookupMode,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence},
    catalog::{
        roles::RoleIdentity,
        security::BoundTableSecurity,
        stored_view::{StoredView, StoredViewDefinition},
    },
};

fn fixture() -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    let uqa_sql::plan::UnifiedPlan::Query(query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("SELECT 'retained' AS n")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    Arc::make_mut(&mut snapshot.definitions.views).insert(
        RelationIdentity::new("public", "v"),
        StoredView {
            security: BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
            definition: StoredViewDefinition {
                object_id: [3; 16],
                query: *query,
                output_columns: Some(vec!["n".into()]),
                persistence: RelationPersistence::Permanent,
                options: vec![("opaque".into(), "x".repeat(4 << 20))],
                kind: crate::catalog::view::StoredViewKind::View,
                materialized_rows: vec![],
                materialized_column_types: vec![],
                materialized_column_collations: None,
                populated: true,
                catalog_oids: None,
                row_type_array_name: None,
            },
        },
    );
    Arc::make_mut(&mut snapshot.definitions.foreign_tables).insert(
        RelationIdentity::new("public", "remote"),
        StoredForeignTable {
            name: "public.remote".into(),
            persistence: RelationPersistence::Permanent,
            object_id: [2; 16],
            catalog_oids: None,
            row_type_array_name: None,
            server_name: "remote_server".into(),
            server_reference: None,
            columns: vec![ColumnDef::nullable("n", ColumnType::Text)],
            dropped_attributes: vec![],
            checks: vec![],
            option_order: vec!["opaque".into()],
            options: [("opaque".into(), "x".repeat(4 << 20))].into(),
        },
    );
    CatalogReadView::new(snapshot)
}

fn namespace() -> RelationNameResolution {
    RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    }
}

#[test]
fn correlation_catalog_facets_borrow_selected_view_and_foreign_declarations() {
    let catalog = fixture();
    let namespace = namespace();
    let view = catalog
        .snapshot()
        .definitions
        .views
        .get(&RelationIdentity::new("public", "v"))
        .unwrap();
    let foreign = catalog
        .snapshot()
        .definitions
        .foreign_tables
        .get(&RelationIdentity::new("public", "remote"))
        .unwrap();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut calls = 0;
    let measured = allocation_counter::measure(|| {
        catalog
            .with_view_definition_with_control(&namespace, "v", &control, &mut |selected| {
                assert!(std::ptr::eq(selected.query, &raw const view.query));
                assert!(std::ptr::eq(
                    selected.output_columns.unwrap(),
                    view.output_columns.as_deref().unwrap()
                ));
                calls += 1;
                Ok(())
            })
            .unwrap();
        catalog
            .with_foreign_table_columns_with_control(
                &namespace,
                "remote",
                &control,
                &mut |columns| {
                    assert!(std::ptr::eq(columns, foreign.columns.as_slice()));
                    calls += 1;
                    Ok(())
                },
            )
            .unwrap();
    });
    assert_eq!(calls, 2);
    assert!(measured.bytes_total < 16 << 10, "{measured:?}");
    assert_eq!(measured.bytes_current, 0);
    assert_eq!(budget.used(), 0);
    let columns = catalog
        .virtual_relation_columns_with_control(&namespace, "pg_catalog.pg_class", &control)
        .unwrap()
        .unwrap();
    assert!(columns.iter().any(|column| column == "relname"));
    assert!(budget.used() > 0);
    drop(columns);
    assert_eq!(budget.used(), 0);
}

#[test]
fn correlation_catalog_selection_rejects_before_callbacks_and_preserves_sibling_leases() {
    let catalog = fixture();
    let namespace = namespace();
    for cancelled in [false, true] {
        let parent = MemoryBudget::new(8192);
        let sibling = parent.reserve(37).unwrap();
        let budget = parent.child(if cancelled { 4096 } else { 0 });
        let token = CancellationToken::new();
        if cancelled {
            token.cancel();
        }
        let control = ProductionControl::new(&budget, &token, &token);
        let error = catalog
            .with_view_definition_with_control(&namespace, "v", &control, &mut |_| {
                panic!("rejected before callback")
            })
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancelled { "57014" } else { "53200" })
        );
        let error = catalog
            .with_foreign_table_columns_with_control(&namespace, "remote", &control, &mut |_| {
                panic!("rejected before callback")
            })
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancelled { "57014" } else { "53200" })
        );
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
}
