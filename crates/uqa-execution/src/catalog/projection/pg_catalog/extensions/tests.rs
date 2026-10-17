//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache,
    projection::dependencies::CatalogDependencies,
    test_support::{empty_catalog, CatalogServices},
};
use uqa_sql::catalog::dependencies::ObjectAddress;

// Independent PostgreSQL 18.4 initdb catalog queries, verified with psql in Docker.
#[test]
fn bundled_extension_rows_and_column_descriptors_match_postgresql() {
    let catalog = empty_catalog();
    assert_eq!(
        build_pg_extension(&catalog),
        vec![row([
            ("oid", Value::Int(13643)),
            ("extname", Value::Str("plpgsql".into())),
            ("extowner", Value::Int(10)),
            ("extnamespace", Value::Int(11)),
            ("extrelocatable", Value::Bool(false)),
            ("extversion", Value::Str("1.0".into())),
            ("extconfig", Value::Null),
            ("extcondition", Value::Null),
        ])]
    );
    let attributes = extension_attribute_rows().unwrap();
    let actual = attributes
        .iter()
        .map(|row| ["attnum", "attname", "atttypid", "attnotnull"].map(|name| row[name].clone()))
        .collect::<Vec<_>>();
    let expected = [
        (1, "oid", 26, true),
        (2, "extname", 19, true),
        (3, "extowner", 26, true),
        (4, "extnamespace", 26, true),
        (5, "extrelocatable", 16, true),
        (6, "extversion", 25, true),
        (7, "extconfig", 1028, false),
        (8, "extcondition", 1009, false),
    ]
    .map(|(number, name, ty, not_null)| {
        [
            Value::Int(number),
            Value::Str(name.into()),
            Value::Int(ty),
            Value::Bool(not_null),
        ]
    });
    assert_eq!(actual, expected);
}

#[test]
fn extension_relation_row_type_and_array_share_postgresql_identities() {
    let catalog = empty_catalog();
    let relation = extension_class_row(&catalog);
    let types = extension_type_rows(&catalog);
    let [composite, array] = types.as_slice() else {
        panic!("composite and companion array")
    };
    assert_eq!(relation["oid"], Value::Int(3079));
    assert_eq!(relation["reltype"], Value::Int(10074));
    assert_eq!(relation["relnatts"], Value::Int(8));
    assert_eq!(composite["oid"], relation["reltype"]);
    assert_eq!(composite["typrelid"], relation["oid"]);
    assert_eq!(composite["typname"], Value::Str("pg_extension".into()));
    assert_eq!(composite["typtype"], Value::Str("c".into()));
    assert_eq!(composite["typarray"], Value::Int(10073));
    assert_eq!(array["oid"], composite["typarray"]);
    assert_eq!(array["typelem"], composite["oid"]);
    assert_eq!(array["typrelid"], Value::Int(0));
    assert_eq!(array["typname"], Value::Str("_pg_extension".into()));
}

#[test]
fn bundled_extension_membership_and_description_match_postgresql() {
    let catalog = empty_catalog();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let dependencies =
        CatalogDependencies::build(&context, &catalog, &services.resolution).unwrap();
    let actual = dependencies
        .depend_rows()
        .into_iter()
        .filter(|row| row["refclassid"] == Value::Int(3079))
        .map(|row| {
            ["classid", "objid", "refclassid", "refobjid", "deptype"].map(|name| row[name].clone())
        })
        .collect::<Vec<_>>();
    let expected =
        [(1255, 13644), (1255, 13645), (1255, 13646), (2612, 13647)].map(|(class, oid)| {
            [
                Value::Int(class),
                Value::Int(oid),
                Value::Int(3079),
                Value::Int(13643),
                Value::Str("e".into()),
            ]
        });
    assert_eq!(actual, expected);
    assert_eq!(
        dependencies
            .describe(&context, ObjectAddress::whole(3079, 13643))
            .unwrap(),
        Some("extension plpgsql".into())
    );
    assert_eq!(
        dependencies
            .describe(&context, ObjectAddress::whole(3079, 0))
            .unwrap(),
        None
    );
}
