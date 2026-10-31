//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    foreign::StoredForeignTable,
    test_support::{empty_catalog, table_snapshot},
};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken, RelationIdentity};
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence},
    catalog::resolution::RelationLookupMode,
    ColumnType,
};

fn namespace() -> RelationNameResolution {
    RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Bound,
    }
}

#[test]
fn selected_relation_names_keep_exact_leases_and_reject_wide_columns_before_copy() {
    let mut snapshot = empty_catalog().snapshot().clone();
    for (name, column) in [
        ("small", "id".into()),
        ("wide", "wide_column_".repeat(256 * 1024)),
    ] {
        snapshot.tables.insert(
            RelationIdentity::new("public", name),
            table_snapshot(
                [1; 16],
                vec![ColumnDef::nullable(column, ColumnType::Text)],
                uqa_sql::ast::TableConstraintSet::default(),
            ),
        );
    }
    let catalog = CatalogReadView::new(snapshot);
    let namespace = namespace();
    let budget = MemoryBudget::new(512);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = budget.reserve(37).unwrap();
    let measured = allocation_counter::measure(|| {
        let error =
            relation(&catalog, &namespace, "public.wide", &control).expect_err("admission failure");
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    assert_eq!(budget.used(), 37);
    let mut selected = None;
    let measured = allocation_counter::measure(|| {
        selected = relation(&catalog, &namespace, "public.small", &control).unwrap();
    });
    let selected = selected.unwrap();
    assert_eq!(selected.canonical, "public.small");
    assert_eq!(selected.columns, ["id"]);
    assert_eq!(
        selected.reserved_bytes(),
        selected.canonical.capacity()
            + selected.columns.capacity() * size_of::<String>()
            + selected.columns.iter().map(String::capacity).sum::<usize>()
    );
    assert_eq!(measured.bytes_current, selected.reserved_bytes() as i64);
    assert_eq!(budget.used(), 37 + selected.reserved_bytes());
    drop(selected);
    assert_eq!(budget.used(), 37);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn foreign_descriptor_selection_borrows_unrelated_options_and_definitions() {
    let mut snapshot = empty_catalog().snapshot().clone();
    let table = StoredForeignTable {
        name: "public.remote".into(),
        persistence: RelationPersistence::Permanent,
        object_id: [2; 16],
        catalog_oids: None,
        row_type_array_name: None,
        server_name: "remote_server".into(),
        server_reference: None,
        columns: vec![ColumnDef::nullable("id", ColumnType::Integer)],
        dropped_attributes: vec![],
        checks: vec![],
        option_order: vec!["payload".into()],
        options: [("payload".into(), "opaque".repeat(512 * 1024))].into(),
    };
    Arc::make_mut(&mut snapshot.definitions.foreign_tables)
        .insert(RelationIdentity::new("public", "remote"), table);
    let catalog = CatalogReadView::new(snapshot);
    let namespace = namespace();
    let old = allocation_counter::measure(|| {
        drop(
            catalog
                .foreign_table_entry_resolved(&namespace, "public.remote")
                .unwrap(),
        );
    });
    assert!(old.bytes_total > 2 * 1024 * 1024);
    let budget = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let measured = allocation_counter::measure(|| {
        let selected = relation(&catalog, &namespace, "public.remote", &control)
            .unwrap()
            .unwrap();
        assert_eq!(selected.columns, ["id"]);
        assert!(matches!(selected.kind, PrivilegeRelationKind::ForeignTable));
    });
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    assert_eq!(budget.used(), 0);
}
