//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::projection::{dependencies::CatalogDependencies, pg_catalog};
use uqa_sql::{
    ast::Expr,
    catalog::dependencies::{CONSTRAINT_CLASS, RELATION_CLASS},
    ColumnType,
};

fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!("check_references.json")).unwrap()
}

fn catalog_fixture(case: &serde_json::Value) -> (CatalogReadView, RelationIdentity) {
    let Statement::CreateTable(mut table) = uqa_sql::compile(case["sql"].as_str().unwrap())
        .unwrap()
        .remove(0)
    else {
        panic!("CREATE TABLE reference");
    };
    // Catalog definitions contain analyzed whole-row variables. SQL owner tests verify this normalization against the same independent reference.
    for check in &mut table.checks {
        uqa_sql::catalog::stored_ast::visit_stored_expression(&mut check.expr, &mut |node| {
            if matches!(node, Expr::Column(name) if name == &table.name) {
                *node = Expr::QualifiedStar(table.name.clone().into());
            }
            Ok(())
        })
        .unwrap();
    }
    let identity = RelationIdentity::new("public", &table.name);
    let mut constraints = TableConstraintSet {
        checks: table.checks,
        ..TableConstraintSet::default()
    };
    let mut object = 500_u128;
    uqa_sql::schema::constraint_metadata::materialize_constraint_metadata(
        &identity,
        &mut table.columns,
        &mut constraints,
        &mut |_: &str| {
            object += 1;
            Ok(object.to_le_bytes())
        },
    )
    .unwrap();
    let mut snapshot = fixture(0).snapshot().clone();
    snapshot.tables.clear();
    snapshot.tables.insert(
        identity.clone(),
        table_snapshot([42; 16], table.columns, constraints),
    );
    (CatalogReadView::new(snapshot), identity)
}

#[test]
fn check_catalog_keys_match_postgresql_whole_rows_system_attributes_and_order() {
    for case in reference()["cases"].as_array().unwrap() {
        let (catalog, _) = catalog_fixture(case);
        let services = CatalogServices::default();
        let rows = pg_catalog::build_pg_constraint(&catalog, &services.resolution).unwrap();
        let mut actual = rows
            .iter()
            .map(|row| {
                let Value::Str(name) = &row["conname"] else {
                    panic!("constraint name");
                };
                let key = (row["conkey"] != Value::Null).then(|| {
                    uqa_sql::result::format_postgres_text(
                        &row["conkey"],
                        &ColumnType::Array(Box::new(ColumnType::SmallInteger)),
                        None,
                    )
                    .unwrap()
                });
                serde_json::json!([name, key])
            })
            .collect::<Vec<_>>();
        actual.sort_by(|left, right| left[0].as_str().cmp(&right[0].as_str()));
        assert_eq!(serde_json::json!(actual), case["keys"]);
        let alias = catalog.clone();
        assert!(std::ptr::eq(
            constraint_catalog_rows(&catalog, &services.resolution).unwrap(),
            constraint_catalog_rows(&alias, &services.resolution).unwrap()
        ));
    }
}

#[test]
fn check_dependencies_match_postgresql_automatic_and_normal_attribute_edges() {
    let reference = reference();
    let case = &reference["cases"][0];
    let (catalog, identity) = catalog_fixture(case);
    let services = CatalogServices::default();
    let output = RegtypeOutputCache::default();
    let context = services.context(&catalog, &output);
    let dependencies =
        CatalogDependencies::build(&context, &catalog, &services.resolution).unwrap();
    let relation = dependencies.relation_address(&identity, None).unwrap();
    let mut actual = Vec::new();
    for constraint in constraint_catalog_rows(&catalog, &services.resolution).unwrap() {
        for edge in dependencies.graph().edges() {
            if edge.dependent.class_id == CONSTRAINT_CLASS
                && i64::from(edge.dependent.object_id) == constraint.catalog_oid.unwrap()
                && edge.referenced.class_id == RELATION_CLASS
                && edge.referenced.object_id == relation.object_id
            {
                actual.push((
                    constraint.name.clone(),
                    edge.referenced.sub_id,
                    edge.kind.code().to_string(),
                ));
            }
        }
    }
    actual.sort();
    let actual = actual
        .into_iter()
        .map(|(name, number, kind)| serde_json::json!([name, number.to_string(), kind]))
        .collect::<Vec<_>>();
    assert_eq!(serde_json::json!(actual), case["dependencies"]);
}
