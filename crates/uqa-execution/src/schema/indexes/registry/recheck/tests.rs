//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::ast::TableConstraintSet;

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
            object_id: [1; 16],
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
