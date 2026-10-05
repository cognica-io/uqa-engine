//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeSet, sync::Arc};
use uqa_sql::{
    ast::Statement,
    routines::{RoutineBody, SQLUserFunction},
};

fn function(identity: u8, revision: u8) -> Arc<SQLUserFunction> {
    let Statement::CreateFunction(mut definition) =
        uqa_sql::compile("CREATE FUNCTION public.f() RETURNS int LANGUAGE sql AS 'SELECT 1'")
            .unwrap()
            .remove(0)
    else {
        panic!("function");
    };
    definition.object_id = Some([identity; 16]);
    definition.catalog_revision = Some([revision; 16]);
    Arc::new(SQLUserFunction::new(*definition, RoutineBody::Source))
}

#[test]
fn snapshots_follow_selected_routine_tuple_identity_and_global_namespaces() {
    let mut snapshot = super::super::test_support::empty_catalog()
        .snapshot()
        .clone();
    Arc::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert("public.f".into(), vec![function(1, 10)]);
    let dependencies = PreparedAnalysisDependencies {
        relations: BTreeSet::new(),
        routines: BTreeSet::from([[1; 16]]),
    };
    let original = capture(&CatalogReadView::new(snapshot.clone()), &dependencies).unwrap();
    Arc::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert("public.other".into(), vec![function(2, 20)]);
    assert_eq!(
        capture(&CatalogReadView::new(snapshot.clone()), &dependencies).unwrap(),
        original
    );
    Arc::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert("public.f".into(), vec![function(1, 11)]);
    let replaced = capture(&CatalogReadView::new(snapshot.clone()), &dependencies).unwrap();
    assert_ne!(replaced, original);
    Arc::make_mut(&mut snapshot.definitions.sql_user_functions).remove("public.f");
    let missing = capture(&CatalogReadView::new(snapshot.clone()), &dependencies).unwrap();
    assert_eq!(missing.routines[&[1; 16]], None);
    Arc::make_mut(&mut snapshot.definitions.schemas).insert(
        "new_namespace".into(),
        super::super::security::BoundSchemaSecurity::bootstrap("new_namespace"),
    );
    let namespace = capture(&CatalogReadView::new(snapshot), &dependencies).unwrap();
    assert_ne!(namespace.global_catalog, missing.global_catalog);
}

#[test]
fn relation_dependencies_resolve_the_bound_identity_and_report_missing_objects() {
    use super::super::{CatalogTableSnapshot, RelationLookupMode, RelationNameResolution};
    use uqa_sql::catalog::{
        analysis::AnalysisCatalog,
        relation_oids::{RelationCatalogOids, RelationOidKind},
    };
    let mut snapshot = super::super::test_support::empty_catalog()
        .snapshot()
        .clone();
    let object = [7; 16];
    let oids = RelationCatalogOids::legacy(RelationOidKind::Table, &object);
    snapshot.tables.insert(
        RelationIdentity::new("public", "docs"),
        CatalogTableSnapshot {
            object_id: object,
            catalog_oids: oids,
            security: Arc::new(super::super::security::BoundTableSecurity::owner(
                uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
            )),
            columns: Arc::default(),
            columns_declared: true,
            checks: Arc::default(),
            foreign_keys: Arc::default(),
            keys: Arc::default(),
            hierarchy: Arc::default(),
            persistence: uqa_sql::ast::RelationPersistence::Permanent,
        },
    );
    let catalog = CatalogReadView::new(snapshot);
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        current_user: "uqa".into(),
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        lookup_mode: RelationLookupMode::Bound,
    };
    assert_eq!(
        catalog
            .relation_dependency(&resolution, "public.docs")
            .unwrap(),
        Some(oids.relation)
    );
    let dependencies = PreparedAnalysisDependencies {
        relations: BTreeSet::from([oids.relation, 0]),
        routines: BTreeSet::new(),
    };
    let revision = capture(&catalog, &dependencies).unwrap();
    assert!(revision.relations[&oids.relation].is_some());
    assert!(revision.relations[&0].is_none());
    let mut dropped = catalog.snapshot().clone();
    dropped.tables.clear();
    assert!(capture(&CatalogReadView::new(dropped), &dependencies)
        .unwrap()
        .relations[&oids.relation]
        .is_none());
}
