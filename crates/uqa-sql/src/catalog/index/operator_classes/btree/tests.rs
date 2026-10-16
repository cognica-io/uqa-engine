//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn builtin_btree_class_identities_match_postgresql_catalog_support() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let rows = reference["classes"].as_array().unwrap();
    assert_eq!(rows.len(), builtin_btree_operator_classes().len());
    let mut oids = std::collections::BTreeSet::new();
    for row in rows {
        let oid = u32::try_from(row["oid"].as_u64().unwrap()).unwrap();
        assert!(oids.insert(oid));
        let actual = builtin_btree_operator_class(oid).unwrap();
        assert_eq!(actual.name, row["name"].as_str().unwrap());
        assert_eq!(actual.default, row["default"].as_bool().unwrap());
        assert_eq!(
            u64::from(actual.family_oid),
            row["family_oid"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(actual.input_type_oid),
            row["input_type_oid"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(actual.comparison_oid),
            row["comparison_oid"].as_u64().unwrap()
        );
    }
    for missing in [0, 1, u32::MAX] {
        assert!(builtin_btree_operator_class(missing).is_none());
    }
}

#[test]
fn default_btree_class_selection_matches_actual_postgresql_indexes() {
    let reference: serde_json::Value = serde_json::from_str(include_str!("defaults.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap().split(';').next().unwrap();
        let crate::Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
            panic!("CREATE TABLE")
        };
        let ty = table.columns[0].ty.clone();
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "selected_domain".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(ty.clone()),
        };
        for ty in [&ty, &domain] {
            let actual = default_btree_operator_class(ty)
                .unwrap_or_else(|| panic!("{}: {ty:?}", case["type"]));
            assert_eq!(
                u64::from(actual.oid),
                case["class_oid"].as_u64().unwrap(),
                "{}",
                case["type"]
            );
            assert_eq!(actual.name, case["class_name"].as_str().unwrap());
            assert_eq!(
                u64::from(actual.family_oid),
                case["family_oid"].as_u64().unwrap()
            );
            assert_eq!(
                u64::from(actual.input_type_oid),
                case["input_type_oid"].as_u64().unwrap()
            );
            assert!(super::super::has_default_btree_operator_class(ty));
        }
    }
    for ty in [
        ColumnType::Json,
        ColumnType::Xid,
        ColumnType::RefCursor,
        ColumnType::AclItem,
        ColumnType::PgNodeTree,
    ] {
        assert!(default_btree_operator_class(&ty).is_none());
    }
    for ty in [ColumnType::Vector(3), ColumnType::Tensor(3)] {
        assert!(
            default_btree_operator_class(&ty).is_none(),
            "no invented PostgreSQL OID"
        );
        assert!(super::super::has_default_btree_operator_class(&ty));
    }
}
