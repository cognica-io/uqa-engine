//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn partition_btree_class_identities_match_postgresql_catalog() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let mut mismatches = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap().split(';').next().unwrap();
        let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile(sql).unwrap().remove(0)
        else {
            panic!("CREATE TABLE")
        };
        let strategy = match case["strategy"].as_str().unwrap() {
            "range" => PartitionStrategy::Range,
            "list" => PartitionStrategy::List,
            _ => panic!("B-tree partition strategy"),
        };
        let ty = table.columns[0].ty.clone();
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "partition_domain".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(ty.clone()),
        };
        for ty in [&ty, &domain] {
            let actual = partition_operator_class(strategy, ty);
            let expected = case["class_oid"].as_i64().unwrap();
            if actual != expected {
                mismatches.push(format!(
                    "{} {}: {actual}, expected {expected}",
                    case["strategy"], case["type"]
                ));
            }
            assert_eq!(
                projected_class(&table, ty),
                catalog_oidvector(vec![Value::Int(expected)], "reference").unwrap(),
                "{} {}",
                case["strategy"],
                case["type"]
            );
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

fn projected_class(table: &uqa_sql::ast::CreateTable, ty: &ColumnType) -> Value {
    use crate::catalog::{
        cache::RegtypeOutputCache,
        test_support::{empty_catalog, table_snapshot, CatalogServices},
    };
    let mut columns = table.columns.clone();
    columns[0].ty = ty.clone();
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        uqa_core::RelationIdentity::new("public", "partition_keys"),
        table_snapshot(
            [1; 16],
            columns,
            uqa_sql::ast::TableConstraintSet {
                hierarchy: table.hierarchy.clone(),
                ..Default::default()
            },
        ),
    );
    let catalog = CatalogReadView::new(snapshot);
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let rows = build_pg_partitioned_table(&context, &catalog, &services.resolution).unwrap();
    assert_eq!(rows.len(), 1);
    rows[0]["partclass"].clone()
}
