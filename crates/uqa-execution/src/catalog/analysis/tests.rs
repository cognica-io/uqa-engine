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

fn source_view_context(
    query: uqa_sql::plan::QueryPlan,
) -> uqa_sql::binding::BindingContext<'static> {
    let mut snapshot = fixture().snapshot().clone();
    Arc::make_mut(&mut snapshot.definitions.views)
        .get_mut(&RelationIdentity::new("public", "v"))
        .unwrap()
        .definition
        .query = query;
    uqa_sql::binding::BindingContext {
        catalog: Arc::new(CatalogReadView::new(snapshot)),
        resolution: namespace(),
        ctes: std::collections::BTreeMap::new(),
        deferred_ctes: std::collections::BTreeMap::new(),
        non_returning_ctes: std::collections::BTreeSet::new(),
        scalar_subqueries: &[],
    }
}

#[test]
fn source_schema_binding_borrows_retained_view_payloads_instead_of_cloning_them() {
    use crate::catalog::test_support::NoRoutines;
    use uqa_sql::plan::{RelationalPlan, UnifiedPlan};
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile(&format!("SELECT '{}'::text AS n", "x".repeat(4 << 20)))
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    // Catalog definitions pass through stored input binding before publication.
    let raw_context = source_view_context(*query.clone());
    uqa_sql::binding::bind_query_plan_routines_for_storage(
        &NoRoutines,
        &mut query,
        &[],
        &raw_context,
        None,
    )
    .unwrap();
    query.visit_scalar_expressions(&mut |expression| {
        assert!(!uqa_sql::type_resolution::contains_unknown_literal(
            expression
        ));
    });
    let context = source_view_context(*query);
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT * FROM v").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let source = block.from.as_ref().unwrap();
    let memory = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    // Unanalyzed definitions still owe input conversion; reject that allocation before copying the payload.
    let rejected = allocation_counter::measure(|| {
        assert_eq!(
            uqa_sql::binding::production::bind_source_plan_schema_with_control(
                &NoRoutines,
                source,
                &[],
                raw_context.copy_with_control(&control).unwrap(),
                None,
                &control,
            )
            .unwrap_err()
            .sqlstate(),
            Some("53200"),
        );
    });
    assert!(rejected.bytes_total < 256 << 10, "{rejected:?}");
    assert_eq!(memory.used(), 0);
    let mut schema = None;
    let allocation = allocation_counter::measure(|| {
        schema = Some(
            uqa_sql::binding::production::bind_source_plan_schema_with_control(
                &NoRoutines,
                source,
                &[],
                context.copy_with_control(&control).unwrap(),
                None,
                &control,
            )
            .unwrap(),
        );
    });
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    let schema = schema.unwrap();
    assert_eq!(schema.column_type(0), Some(&ColumnType::Text));
    assert!(memory.used() > 0);
    drop(schema);
    assert_eq!(memory.used(), 0);
    for token in [&original, &invoking] {
        let scope = context.copy_with_control(&control).unwrap();
        token.cancel();
        assert_eq!(
            uqa_sql::binding::production::bind_source_plan_schema_with_control(
                &NoRoutines,
                source,
                &[],
                scope,
                None,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        token.reset();
    }
}

#[test]
fn admitted_source_catalog_adapters_preserve_shadowing_row_type_identity_and_cleanup() {
    let mut snapshot = fixture().snapshot().clone();
    Arc::make_mut(&mut snapshot.definitions.sequences).insert(
        RelationIdentity::new("later", "v"),
        crate::catalog::sequence::SequenceState::initial(
            1,
            1,
            uqa_sql::ast::SequenceDataType::BigInt,
        ),
    );
    let catalog = CatalogReadView::new(snapshot);
    let mut namespace = namespace();
    namespace.search_path.push("later".into());
    let memory = MemoryBudget::new(128 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let owned: Arc<dyn AnalysisCatalog> = control
        .finish(catalog.clone(), control.empty_reservation())
        .unwrap()
        .into_shared(&control)
        .unwrap();
    let baseline = memory.used();
    assert!(!owned
        .sequence_exists_with_control(&namespace, "v", &control)
        .unwrap());
    assert!(owned
        .sequence_exists_with_control(&namespace, "later.v", &control)
        .unwrap());
    for name in ["v", "remote", "later.v"] {
        let expected = catalog.relation_row_type(&namespace, name).unwrap();
        let selected = owned
            .relation_row_type_with_control(&namespace, name, &control)
            .unwrap();
        assert_eq!(selected.as_deref(), expected.as_ref());
        drop(selected);
        assert_eq!(memory.used(), baseline);
    }
    let columns = owned
        .virtual_relation_schema_with_control(&namespace, "pg_catalog.pg_class", &control)
        .unwrap()
        .unwrap();
    assert!(columns
        .iter()
        .any(|(name, ty)| name == "relacl"
            && *ty == ColumnType::Array(Box::new(ColumnType::AclItem))));
    assert!(memory.used() > baseline);
    drop(columns);
    assert_eq!(memory.used(), baseline);
    for cancelled in 0..3 {
        let occupied = (cancelled == 0).then(|| memory.reserve(memory.available()).unwrap());
        if cancelled == 1 {
            original.cancel();
        }
        if cancelled == 2 {
            invoking.cancel();
        }
        let state = if cancelled == 0 { "53200" } else { "57014" };
        assert_eq!(
            owned
                .relation_row_type_with_control(&namespace, "v", &control)
                .unwrap_err()
                .sqlstate(),
            Some(state)
        );
        assert_eq!(
            owned
                .sequence_exists_with_control(&namespace, "later.v", &control)
                .unwrap_err()
                .sqlstate(),
            Some(state)
        );
        assert_eq!(
            owned
                .virtual_relation_schema_with_control(&namespace, "pg_catalog.pg_class", &control)
                .unwrap_err()
                .sqlstate(),
            Some(state)
        );
        assert_eq!(
            memory.used(),
            baseline
                + occupied
                    .as_ref()
                    .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        drop(occupied);
        original.reset();
        invoking.reset();
    }
    drop(owned);
    assert_eq!(memory.used(), 0);
}
