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
    ast::{RelationPersistence, Statement, TableConstraintSet},
    catalog::{
        range_type::{StoredMultirange, StoredRange},
        roles::RoleIdentity,
        security::BoundSchemaSecurity,
    },
    routines::{RoutineBody, SQLUserFunction},
    ColumnType,
};

fn check(catalog: &CatalogReadView) -> Result<(), SQLError> {
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    let result = validate(
        catalog,
        &ProductionControl::new(&memory, &cancellation, &cancellation),
    );
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_namespaces_check_exact_membership_without_creating_public_or_temporary_schemas() {
    for name in [
        "public",
        "missing",
        "pg_temp_1",
        "pg_toast_temp_1",
        "app.quoted",
    ] {
        let mut snapshot = empty_catalog().snapshot().clone();
        snapshot.tables.insert(
            RelationIdentity::new(name, "items"),
            table_snapshot([1; 16], vec![], TableConstraintSet::default()),
        );
        assert!(check(&CatalogReadView::new(snapshot.clone())).is_err());
        RetainedValue::make_mut(&mut snapshot.definitions.schemas)
            .insert(name.into(), BoundSchemaSecurity::bootstrap(name))
            .unwrap();
        check(&CatalogReadView::new(snapshot.clone())).unwrap();
        RetainedValue::make_mut(&mut snapshot.definitions.schemas)
            .remove(name)
            .unwrap();
        RetainedValue::make_mut(&mut snapshot.definitions.schemas)
            .insert(
                format!("{name}.other"),
                BoundSchemaSecurity::bootstrap(name),
            )
            .unwrap();
        assert!(check(&CatalogReadView::new(snapshot)).is_err());
    }
}

#[test]
fn native_namespaces_preserve_virtual_system_schemas_and_native_graph_namespaces() {
    let mut snapshot = empty_catalog().snapshot().clone();
    RetainedValue::make_mut(&mut snapshot.definitions.graphs)
        .insert(
            "native_graph",
            std::sync::Arc::new(uqa_graph::GraphStoreHandle::default()),
        )
        .unwrap();
    for name in [
        "pg_catalog",
        "information_schema",
        "ag_catalog",
        "native_graph",
    ] {
        snapshot.tables.insert(
            RelationIdentity::new(name, "items"),
            table_snapshot([1; 16], vec![], TableConstraintSet::default()),
        );
    }
    assert!(snapshot.definitions.schemas.is_empty());
    check(&CatalogReadView::new(snapshot.clone())).unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.graphs)
        .remove("native_graph")
        .unwrap();
    assert!(check(&CatalogReadView::new(snapshot)).is_err());
}

#[test]
fn native_namespaces_check_indexes_and_independently_relocated_multiranges() {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas = RetainedValue::new(BoundSchemaSecurity::initial_catalog());
    RetainedValue::make_mut(&mut snapshot.definitions.catalog_indexes)
        .insert(
            uqa_core::catalog_index::CatalogIndexRow {
                relation: RelationIdentity::new("other", "saved"),
                index_type: "btree".into(),
                table_name: "public.items".into(),
                columns_json: "[]".into(),
                parameters_json: "{}".into(),
                definition_json: None,
            },
            RelationPersistence::Permanent,
        )
        .unwrap();
    assert!(check(&CatalogReadView::new(snapshot.clone())).is_err());
    RetainedValue::make_mut(&mut snapshot.definitions.schemas)
        .insert("other".into(), BoundSchemaSecurity::bootstrap("other"))
        .unwrap();
    check(&CatalogReadView::new(snapshot.clone())).unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.ranges)
        .insert(
            "public.reading".into(),
            StoredRange {
                object_id: [3; 16],
                oid: 30_000,
                array_oid: 30_001,
                array_name: "_reading".into(),
                identity: RelationIdentity::new("public", "reading"),
                owner: RoleIdentity::BOOTSTRAP,
                subtype: ColumnType::Integer,
                subtype_operator_class: 1_978,
                collation_oid: 0,
                canonical_oid: None,
                subtype_difference_oid: None,
                multirange: StoredMultirange {
                    object_id: [4; 16],
                    oid: 30_002,
                    array_oid: 30_003,
                    array_name: "_readings".into(),
                    identity: RelationIdentity::new("relocated", "readings"),
                },
                usage_acl: None,
            },
        )
        .unwrap();
    assert!(check(&CatalogReadView::new(snapshot.clone())).is_err());
    RetainedValue::make_mut(&mut snapshot.definitions.schemas)
        .insert(
            "relocated".into(),
            BoundSchemaSecurity::bootstrap("relocated"),
        )
        .unwrap();
    check(&CatalogReadView::new(snapshot)).unwrap();
}

#[test]
fn native_namespaces_admit_routine_name_decoding_and_preserve_lazy_source_bodies() {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas = RetainedValue::new(BoundSchemaSecurity::initial_catalog());
    let Statement::CreateFunction(function) = uqa_sql::compile(
        "CREATE FUNCTION public.f() RETURNS integer LANGUAGE SQL AS 'SELECT missing()'",
    )
    .unwrap()
    .remove(0) else {
        panic!("function");
    };
    RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert(
            "public.f".into(),
            vec![SQLUserFunction::new(*function, RoutineBody::Source).shared()],
        )
        .unwrap();
    let catalog = CatalogReadView::new(snapshot);
    check(&catalog).unwrap();
    let cancellation = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    assert!(validate(
        &catalog,
        &ProductionControl::new(&empty, &cancellation, &cancellation)
    )
    .is_err());
    assert_eq!(empty.used(), 0);
    let memory = MemoryBudget::new(64 << 10);
    cancellation.cancel();
    assert!(validate(
        &catalog,
        &ProductionControl::new(&memory, &cancellation, &cancellation)
    )
    .is_err());
    assert_eq!(memory.used(), 0);
    assert_eq!(catalog.snapshot().definitions.sql_user_functions.len(), 1);
    check(&catalog).unwrap();
}
