//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;

#[test]
fn relation_descriptors_preserve_selected_collations_and_attribute_numbers() {
    let uqa_sql::Statement::CreateTable(mut table) =
        uqa_sql::compile("CREATE TABLE t(c text,removed integer,p text,plain text,n integer)")
            .unwrap()
            .remove(0)
    else {
        panic!("table");
    };
    table.columns[0].collation_oid = Some(950);
    for (index, column) in table.columns.iter_mut().enumerate() {
        column.attribute_number = Some(i16::try_from(index + 1).unwrap());
    }
    let removed = table.columns.remove(1);
    table.columns[1].collation_oid = Some(951);
    let mut constraints = uqa_sql::ast::TableConstraintSet::default();
    constraints.dropped_attributes.push(
        uqa_sql::catalog::relation_attributes::DroppedAttribute::from_column(&removed, 1, true, 0)
            .unwrap(),
    );
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    let table = crate::catalog::test_support::table_snapshot([1; 16], table.columns, constraints);
    let oid = table.catalog_oids.row_type.unwrap();
    let foreign = crate::catalog::foreign::StoredForeignTable {
        name: "f".into(),
        persistence: uqa_sql::ast::RelationPersistence::Permanent,
        object_id: [2; 16],
        catalog_oids: None,
        row_type_array_name: None,
        server_name: "fixture".into(),
        server_reference: None,
        columns: table.columns.to_vec(),
        dropped_attributes: table.dropped_attributes.to_vec(),
        checks: Vec::new(),
        options: BTreeMap::new(),
        option_order: Vec::new(),
    };
    let foreign_oid = foreign.relation_oids().row_type.unwrap();
    Arc::make_mut(&mut snapshot.definitions.foreign_tables)
        .insert(RelationIdentity::new("public", "f"), foreign);
    snapshot
        .tables
        .insert(RelationIdentity::new("public", "t"), table);
    let catalog = CatalogReadView::new(snapshot);
    let services = crate::catalog::test_support::CatalogServices::default();
    let cache = crate::catalog::cache::RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let selected = descriptor(&context, oid).unwrap().unwrap();
    assert_eq!(
        selected
            .attributes
            .iter()
            .map(CompositeAttribute::collation_oid)
            .collect::<Vec<_>>(),
        [950, 951, 100, 0]
    );
    assert_eq!(selected.attributes[1].number, 3);
    assert_eq!(
        descriptor(&context, foreign_oid)
            .unwrap()
            .unwrap()
            .attributes,
        selected.attributes
    );
    assert!(Arc::ptr_eq(
        &selected,
        &descriptor(&context, oid).unwrap().unwrap()
    ));
}

#[test]
fn view_descriptors_preserve_analyzed_output_collations() {
    let uqa_sql::plan::UnifiedPlan::Query(query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("SELECT NULL::text COLLATE \"C\" AS c, NULL::text COLLATE \"POSIX\" AS p")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let view = uqa_sql::catalog::stored_view::StoredView {
        security: uqa_sql::catalog::security::BoundTableSecurity::owner(
            uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
        ),
        definition: uqa_sql::catalog::stored_view::StoredViewDefinition {
            object_id: [3; 16],
            catalog_oids: None,
            row_type_array_name: None,
            query: *query,
            output_columns: Some(vec!["renamed_c".into(), "renamed_p".into()]),
            persistence: uqa_sql::ast::RelationPersistence::Permanent,
            options: Vec::new(),
            kind: uqa_sql::catalog::view::StoredViewKind::View,
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            populated: true,
        },
    };
    let oid = view.relation_oids().row_type.unwrap();
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    Arc::make_mut(&mut snapshot.definitions.views)
        .insert(RelationIdentity::new("public", "v"), view.clone());
    let catalog = CatalogReadView::new(snapshot);
    let services = crate::catalog::test_support::CatalogServices::default();
    let cache = crate::catalog::cache::RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let descriptor = descriptor(&context, oid).unwrap().unwrap();
    assert_eq!(
        descriptor
            .attributes
            .iter()
            .map(|attribute| (attribute.name.as_str(), attribute.collation_oid()))
            .collect::<Vec<_>>(),
        [("renamed_c", 950), ("renamed_p", 951)]
    );
    let columns = crate::catalog::projection::view_columns_for(
        &context,
        &catalog,
        &services.resolution,
        &view,
    )
    .unwrap();
    assert_eq!(
        columns
            .iter()
            .map(|column| column.collation_oid)
            .collect::<Vec<_>>(),
        [Some(950), Some(951)]
    );
}

#[test]
fn relation_descriptor_cache_retains_results_and_bounds_missing_identities() {
    let cache = RelationDescriptorCache::default();
    let builds = Cell::new(0);
    let first = cache
        .get_or_try_init(42, || {
            builds.set(builds.get() + 1);
            Ok(Some(Arc::new(CompositeTypeDescriptor {
                type_oid: 42,
                relation_oid: 43,
                attributes: Vec::new(),
                dropped: Vec::new(),
            })))
        })
        .unwrap()
        .unwrap();
    for _ in 0..32 {
        let hit = cache
            .get_or_try_init(42, || {
                builds.set(builds.get() + 1);
                Ok(None)
            })
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &hit));
    }
    assert_eq!(builds.get(), 1);
    for oid in 100..200 {
        for _ in 0..2 {
            assert!(cache
                .get_or_try_init(oid, || {
                    builds.set(builds.get() + 1);
                    Ok(None)
                })
                .unwrap()
                .is_none());
        }
    }
    assert_eq!(builds.get(), 101);
    let entries = cache.entries.lock();
    assert_eq!(entries.known.len(), 1);
    assert_eq!(entries.last_missing, Some(199));
}

#[test]
fn relation_descriptor_errors_and_new_generations_rebuild() {
    let original = crate::catalog::test_support::empty_catalog();
    let retained = original.clone();
    let replacement = CatalogReadView::new(original.snapshot().clone());
    assert!(original
        .relation_descriptors
        .get_or_try_init(7, || Err(SQLError::Internal("invalid attribute".into())))
        .is_err());
    let descriptor = original
        .relation_descriptors
        .get_or_try_init(7, || {
            Ok(Some(Arc::new(CompositeTypeDescriptor {
                type_oid: 7,
                relation_oid: 8,
                attributes: Vec::new(),
                dropped: Vec::new(),
            })))
        })
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(
        &descriptor,
        &retained
            .relation_descriptors
            .get_or_try_init(7, || panic!("retained descriptor rebuilt"))
            .unwrap()
            .unwrap()
    ));
    assert!(replacement
        .relation_descriptors
        .get_or_try_init(7, || Ok(None))
        .unwrap()
        .is_none());
}
