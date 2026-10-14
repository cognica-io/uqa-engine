//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::ast::TableConstraintSet;
use uqa_sql::catalog::index::IndexRelationships;

fn fixture() -> (RelationIdentity, CatalogReadView) {
    let relation = RelationIdentity::new("public", "t");
    let uqa_sql::Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE t(v int CONSTRAINT one UNIQUE)")
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        key_constraints: table.key_constraints,
        ..Default::default()
    };
    let mut next = 1_u8;
    let mut allocate = |_: &str| {
        next += 1;
        Ok([next; 16])
    };
    uqa_sql::schema::constraint_metadata::materialize_constraint_metadata(
        &relation,
        &mut columns,
        &mut constraints,
        &mut allocate,
    )
    .unwrap();
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    snapshot.tables.insert(
        relation.clone(),
        CatalogTableSnapshot {
            dropped_attributes: std::sync::Arc::new(Vec::new()),
            row_type_array_name: None,
            object_id: [1; 16],
            catalog_oids: uqa_sql::catalog::relation_oids::RelationCatalogOids::legacy(
                uqa_sql::catalog::relation_oids::RelationOidKind::Table,
                &[1; 16],
            ),
            security: Arc::new(crate::catalog::security::BoundTableSecurity::owner(
                uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
            )),
            columns: columns.clone().into(),
            columns_declared: true,
            checks: Arc::default(),
            foreign_keys: Arc::default(),
            keys: constraints.key_constraints.clone().into(),
            hierarchy: Arc::default(),
            persistence: uqa_sql::ast::RelationPersistence::Permanent,
        },
    );
    let change = super::super::constraints::prepare(
        &CatalogReadView::new(snapshot.clone()),
        "public.t",
        [1; 16],
        &columns,
        &constraints,
        &mut allocate,
    )
    .unwrap();
    snapshot.definitions.catalog_indexes = change
        .upserts
        .into_iter()
        .map(|row| (row.relation.clone(), row))
        .collect::<BTreeMap<_, _>>()
        .into();
    (relation, CatalogReadView::new(snapshot))
}

#[test]
fn refresh_accepts_only_name_changes_on_the_same_unmodified_index() {
    let (root, before) = fixture();
    let rows = &before.snapshot().definitions.catalog_indexes;
    let old = RelationIdentity::new("public", "one");
    let new = RelationIdentity::new("public", "renamed");
    let mut refreshed = before.snapshot().clone();
    let current_rows = Arc::make_mut(&mut refreshed.definitions.catalog_indexes);
    let mut renamed = current_rows.remove(&old).unwrap();
    renamed.relation = new.clone();
    current_rows.insert(new.clone(), renamed);
    Arc::make_mut(&mut refreshed.tables.get_mut(&root).unwrap().keys)[0].name =
        Some(new.name.clone());
    validate_snapshots(
        &before,
        &before,
        rows,
        &root,
        &CatalogReadView::new(refreshed.clone()),
    )
    .unwrap();
    for mutation in 0..3 {
        let mut changed = refreshed.clone();
        match mutation {
            0 => changed.tables.get_mut(&root).unwrap().object_id = [99; 16],
            1 => Arc::make_mut(&mut changed.tables.get_mut(&root).unwrap().keys)[0]
                .columns
                .push("other".into()),
            _ => {
                let row = Arc::make_mut(&mut changed.definitions.catalog_indexes)
                    .get_mut(&new)
                    .unwrap();
                let mut definition = super::super::index_definition(row).unwrap();
                definition.catalog.as_mut().unwrap().identity.object_id = [99; 16];
                row.definition_json = Some(serde_json::to_string(&definition).unwrap());
            }
        }
        let error = validate_snapshots(
            &before,
            &before,
            rows,
            &root,
            &CatalogReadView::new(changed),
        )
        .unwrap_err();
        assert_eq!(
            uqa_sql::catalog::errors::storage_error("test", &error).sqlstate(),
            Some("40001")
        );
    }
    let mut removal = rows.as_ref().clone();
    removal.remove(&old);
    let error = validate_snapshots(
        &before,
        &before,
        &removal,
        &root,
        &CatalogReadView::new(refreshed),
    )
    .unwrap_err();
    assert_eq!(
        uqa_sql::catalog::errors::storage_error("test", &error).sqlstate(),
        Some("40001")
    );
}

#[test]
fn catalog_recheck_validates_metadata_without_planning_physical_builds() {
    let (root, before) = fixture();
    let rows = &before.snapshot().definitions.catalog_indexes;
    super::super::builds::PHYSICAL_BUILD_SELECTIONS.set(0);
    validate_snapshots(&before, &before, rows, &root, &before).unwrap();
    assert_eq!(
        super::super::builds::PHYSICAL_BUILD_SELECTIONS.get(),
        0,
        "a catalog recheck constructed an unused physical build plan"
    );

    let mut invalid = before.snapshot().clone();
    Arc::make_mut(&mut invalid.definitions.catalog_indexes)
        .values_mut()
        .next()
        .unwrap()
        .definition_json = Some("invalid JSON".into());
    assert!(validate_snapshots(
        &before,
        &before,
        rows,
        &root,
        &CatalogReadView::new(invalid)
    )
    .is_err());
}

#[test]
fn catalog_recheck_partition_validation_visits_unrelated_tables_once() {
    for unrelated in [0, 60] {
        let (root, catalog) = fixture();
        let mut snapshot = catalog.snapshot().clone();
        let original = snapshot
            .definitions
            .catalog_indexes
            .values()
            .next()
            .unwrap()
            .clone();
        let uqa_sql::Statement::CreateTable(plain) =
            uqa_sql::compile("CREATE TABLE unrelated(v int)")
                .unwrap()
                .remove(0)
        else {
            panic!("table")
        };
        for index in 0..unrelated {
            let relation = RelationIdentity::new("public", format!("unrelated_{index}"));
            let mut table = snapshot.tables[&root].clone();
            table.object_id = [32 + index as u8; 16];
            table.catalog_oids = uqa_sql::catalog::relation_oids::RelationCatalogOids::legacy(
                uqa_sql::catalog::relation_oids::RelationOidKind::Table,
                &table.object_id,
            );
            table.columns = Arc::new(plain.columns.clone());
            table.keys = Arc::default();
            let mut row = original.clone();
            row.relation = RelationIdentity::new("public", format!("unrelated_index_{index}"));
            row.table_name = relation.qualified_name();
            let mut definition = super::super::index_definition(&row).unwrap();
            let identity = definition.catalog.as_mut().unwrap();
            identity.identity.object_id = [96 + index as u8; 16];
            identity.identity.oid = 100_000 + index as i64;
            identity.table_object_id = table.object_id;
            identity.physical_key = format!("physical:{index}");
            definition.relationships = IndexRelationships::default();
            row.definition_json = Some(serde_json::to_string(&definition).unwrap());
            snapshot.tables.insert(relation, table);
            Arc::make_mut(&mut snapshot.definitions.catalog_indexes)
                .insert(row.relation.clone(), row);
        }
        let catalog = CatalogReadView::new(snapshot);
        super::super::validation::PARTITION_TABLE_VISITS.set(0);
        validate_snapshots(
            &catalog,
            &catalog,
            &catalog.snapshot().definitions.catalog_indexes,
            &root,
            &catalog,
        )
        .unwrap();
        assert_eq!(
            super::super::validation::PARTITION_TABLE_VISITS.get(),
            catalog.snapshot().tables.len(),
            "partition validation rescanned every table for every unrelated index"
        );
    }
}

#[test]
fn catalog_recheck_keeps_missing_partition_index_errors() {
    let (root, catalog) = fixture();
    let uqa_sql::Statement::CreateTable(parent) =
        uqa_sql::compile("CREATE TABLE t(v int UNIQUE) PARTITION BY LIST(v)")
            .unwrap()
            .remove(0)
    else {
        panic!("partitioned table")
    };
    let mut snapshot = catalog.snapshot().clone();
    snapshot.tables.get_mut(&root).unwrap().hierarchy = Arc::new(parent.hierarchy);
    let child = RelationIdentity::new("public", "child");
    let child_table = crate::catalog::test_support::table_snapshot(
        [20; 16],
        parent.columns,
        TableConstraintSet {
            hierarchy: uqa_sql::ast::TableHierarchy {
                parents: vec![root.qualified_name()],
                partition_bound: Some(uqa_sql::ast::PartitionBound::List(vec![
                    uqa_sql::ast::Expr::Literal(uqa_core::Value::Int(1)),
                ])),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    snapshot.tables.insert(child, child_table);
    let catalog = CatalogReadView::new(snapshot);
    let error = validate_snapshots(
        &catalog,
        &catalog,
        &catalog.snapshot().definitions.catalog_indexes,
        &root,
        &catalog,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("partition has no attached child index"),
        "{error}"
    );
}
