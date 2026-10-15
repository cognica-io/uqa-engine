//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot, CatalogServices};
use std::sync::Arc;
use uqa_core::catalog_identity::CatalogObjectIdentity;
use uqa_sql::{ast::TableConstraintSet, catalog::roles::RoleDefinition, Statement};

fn fixture() -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas =
        Arc::new(crate::catalog::security::BoundSchemaSecurity::initial_catalog());
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 20_000;
    reader.object_id = [1; 16];
    reader.attributes.clear();
    snapshot.definitions.roles = Arc::new(
        [
            ("uqa".into(), RoleDefinition::bootstrap()),
            (reader.name.clone(), reader),
        ]
        .into(),
    );
    let Statement::CreateTable(mut table) = uqa_sql::compile(
        "CREATE TABLE subject(n integer CONSTRAINT required NOT NULL, \
         CONSTRAINT repeated CHECK(n>0 AND n<10), \
         CONSTRAINT system_attribute CHECK(tableoid<>0), \
         CONSTRAINT fixed CHECK(true))",
    )
    .unwrap()
    .remove(0) else {
        panic!("table")
    };
    table.columns[0].not_null_identity = Some(CatalogObjectIdentity {
        object_id: [2; 16],
        oid: 30_000,
    });
    for (index, check) in table.checks.iter_mut().enumerate() {
        check.object_id = Some([u8::try_from(index + 3).unwrap(); 16]);
        check.catalog_oid = Some(i64::try_from(index + 30_001).unwrap());
    }
    snapshot.tables.insert(
        RelationIdentity::new("public", "subject"),
        table_snapshot(
            [9; 16],
            table.columns,
            TableConstraintSet {
                checks: table.checks,
                ..TableConstraintSet::default()
            },
        ),
    );
    CatalogReadView::new(snapshot)
}

#[test]
fn constraint_usage_preserves_distinct_columns_system_attributes_and_owner_visibility() {
    let catalog = fixture();
    let mut resolution = CatalogServices::default().resolution;
    let mut request = CatalogRequest::default();
    let rows = build(&catalog, &resolution, &request, Usage::Columns).unwrap();
    let pairs = rows
        .iter()
        .map(|row| (row["constraint_name"].clone(), row["column_name"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        [
            (str_value("required"), str_value("n")),
            (str_value("repeated"), str_value("n")),
            (str_value("system_attribute"), str_value("tableoid")),
        ]
    );
    assert!(build(&catalog, &resolution, &request, Usage::Tables)
        .unwrap()
        .is_empty());
    request.require_name("column_name".into(), "tableoid".into());
    assert_eq!(
        build(&catalog, &resolution, &request, Usage::Columns).unwrap(),
        rows[2..]
    );
    resolution.current_user = "reader".into();
    assert!(build(&catalog, &resolution, &request, Usage::Columns)
        .unwrap()
        .is_empty());
}
