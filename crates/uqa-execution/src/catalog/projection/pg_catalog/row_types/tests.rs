//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::empty_catalog;
use uqa_core::Value;

#[test]
fn row_and_array_metadata_retain_their_distinct_catalog_identities() {
    let catalog = empty_catalog();
    let identity = RelationIdentity::new("public", "entry");
    let oids = RelationCatalogOids {
        relation: 20_001,
        row_type: Some(20_003),
        array_type: Some(20_002),
        rule: None,
    };
    let mut rows = Vec::new();
    append_rows(
        &mut rows,
        &catalog,
        &identity,
        oids,
        RoleIdentity::BOOTSTRAP,
        Some("__entry_1"),
    );
    assert_eq!(rows.len(), 2);
    let [row, array] = rows.as_slice() else {
        unreachable!();
    };
    for (field, expected) in [
        ("oid", 20_003),
        ("typrelid", 20_001),
        ("typarray", 20_002),
        ("typelem", 0),
        ("typinput", 2290),
        ("typoutput", 2291),
        ("typsubscript", 0),
    ] {
        assert_eq!(row[field], Value::Int(expected), "{field}");
    }
    for (field, expected) in [
        ("oid", 20_002),
        ("typrelid", 0),
        ("typarray", 0),
        ("typelem", 20_003),
        ("typinput", 750),
        ("typoutput", 751),
        ("typsubscript", 6179),
    ] {
        assert_eq!(array[field], Value::Int(expected), "{field}");
    }
    assert_eq!(row["typname"], Value::Str("entry".into()));
    assert_eq!(array["typname"], Value::Str("__entry_1".into()));
    assert_eq!(row["typowner"], array["typowner"]);
    assert_eq!(row["typacl"], Value::Null);
    assert_eq!(array["typacl"], Value::Null);
}
