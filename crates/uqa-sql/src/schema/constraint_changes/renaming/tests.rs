//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn constraint_renaming_preserves_identity_and_other_definition_fields() {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE t(v integer CONSTRAINT nn NOT NULL CONSTRAINT ck CHECK(v>0), CONSTRAINT tc CHECK(v<10))").unwrap().remove(0) else { panic!("table declaration") };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        checks: table.checks,
        ..Default::default()
    };
    let mut next = 0_u8;
    crate::schema::constraint_metadata::materialize_constraint_metadata(
        &uqa_core::RelationIdentity::new("public", "t"),
        &mut columns,
        &mut constraints,
        &mut |_: &str| {
            next += 1;
            Ok([next; 16])
        },
    )
    .unwrap();
    let original_columns = columns.clone();
    let original_constraints = constraints.clone();
    assert_eq!(
        rename_inherited_constraint("t", &mut columns, &mut constraints, "missing", "nn")
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
    for (from, to) in [
        ("nn", "renamed_nn"),
        ("ck", "renamed_ck"),
        ("tc", "renamed_tc"),
    ] {
        rename_inherited_constraint("t", &mut columns, &mut constraints, from, to).unwrap();
        rename_inherited_constraint("t", &mut columns, &mut constraints, to, from).unwrap();
    }
    assert_eq!(
        serde_json::to_value(columns).unwrap(),
        serde_json::to_value(original_columns).unwrap()
    );
    assert_eq!(
        serde_json::to_value(constraints).unwrap(),
        serde_json::to_value(original_constraints).unwrap()
    );
}

#[test]
fn inherited_rename_diagnostics_require_the_complete_parent_scope() {
    assert_eq!(
        ensure_recursive_rename("nn", false, true)
            .unwrap_err()
            .sqlstate(),
        Some("42P16")
    );
    ensure_recursive_rename("nn", true, true).unwrap();
    ensure_recursive_rename("nn", false, false).unwrap();
    assert_eq!(
        ensure_rename_parents("nn", 2, 1).unwrap_err().sqlstate(),
        Some("42P16")
    );
    ensure_rename_parents("nn", 2, 2).unwrap();
    ensure_rename_parents("nn", 0, 0).unwrap();
}

#[test]
fn foreign_key_rename_preserves_row_and_enforcement_identities_and_partition_provenance() {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE child(a integer CONSTRAINT inline_fk REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED, b integer, CONSTRAINT table_fk FOREIGN KEY(b) REFERENCES parent(id))").unwrap().remove(0) else { panic!("table declaration") };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        foreign_keys: table.foreign_keys,
        ..Default::default()
    };
    let mut next = 0;
    crate::schema::constraint_metadata::materialize_constraint_metadata(
        &uqa_core::RelationIdentity::new("public", "child"),
        &mut columns,
        &mut constraints,
        &mut |_: &str| {
            next += 1;
            Ok([next; 16])
        },
    )
    .unwrap();
    constraints.hierarchy.partition_inherited_foreign_keys = constraints.foreign_keys.clone();
    let original = serde_json::to_value((&columns, &constraints)).unwrap();
    assert!(!rename_foreign_key(
        "child",
        &mut columns,
        &mut constraints,
        "missing",
        "inline_fk"
    )
    .unwrap());
    assert_eq!(
        rename_foreign_key(
            "child",
            &mut columns,
            &mut constraints,
            "inline_fk",
            "table_fk"
        )
        .unwrap_err()
        .sqlstate(),
        Some("42710")
    );
    assert_eq!(
        serde_json::to_value((&columns, &constraints)).unwrap(),
        original
    );
    for name in ["inline_fk", "table_fk"] {
        assert!(
            rename_foreign_key("child", &mut columns, &mut constraints, name, "renamed").unwrap()
        );
        crate::schema::constraint_metadata::identity::foreign_keys::validate(
            &columns,
            &constraints,
        )
        .unwrap();
        assert!(
            rename_foreign_key("child", &mut columns, &mut constraints, "renamed", name).unwrap()
        );
    }
    assert_eq!(
        serde_json::to_value((&columns, &constraints)).unwrap(),
        original
    );
}
