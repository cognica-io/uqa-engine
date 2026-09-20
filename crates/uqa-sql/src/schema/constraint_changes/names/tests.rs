//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn names_retain_each_catalog_row_identity_and_location() {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE t(a int CONSTRAINT nn NOT NULL CONSTRAINT cc CHECK(a>0) CONSTRAINT cf REFERENCES parent(v), b int, CONSTRAINT tc CHECK(b>0), CONSTRAINT tf FOREIGN KEY(b) REFERENCES parent(v), CONSTRAINT uk UNIQUE(a))").unwrap().remove(0) else { panic!("table declaration") };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        checks: table.checks,
        foreign_keys: table.foreign_keys,
        key_constraints: table.key_constraints,
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
    let entries = ConstraintNames::from_definition(&columns, &constraints)
        .entries()
        .map(|entry| (entry.name, entry.object_id, entry.location))
        .collect::<Vec<_>>();
    let reference = columns[0].references.as_ref().unwrap();
    assert_eq!(
        entries,
        vec![
            (
                "nn",
                columns[0].not_null_identity.map(|id| id.object_id),
                ConstraintLocation::NotNull(0)
            ),
            (
                "cc",
                columns[0].check_object_id,
                ConstraintLocation::ColumnCheck(0)
            ),
            (
                "cf",
                reference.catalog_identity.map(|id| id.object_id),
                ConstraintLocation::ColumnForeignKey(0)
            ),
            (
                "tc",
                constraints.checks[0].object_id,
                ConstraintLocation::TableCheck(0)
            ),
            (
                "tf",
                constraints.foreign_keys[0]
                    .catalog_identity
                    .map(|id| id.object_id),
                ConstraintLocation::TableForeignKey(0)
            ),
            (
                "uk",
                constraints.key_constraints[0]
                    .catalog_identity
                    .map(|id| id.object_id),
                ConstraintLocation::Key(0)
            ),
        ]
    );
    assert!(entries.iter().all(|entry| entry.1.is_some()));
    assert_ne!(entries[2].1, reference.object_id);
    assert_ne!(entries[4].1, constraints.foreign_keys[0].object_id);
    let identities = entries
        .iter()
        .map(|entry| entry.1)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(identities.len(), entries.len());
}

#[test]
fn metadata_names_share_the_event_constraint_namespace() {
    for explicit in [false, true] {
        let sql = if explicit {
            "CREATE TABLE t(a int CONSTRAINT t_a_check CHECK(a>0))"
        } else {
            "CREATE TABLE t(a int NOT NULL CHECK(a>0) UNIQUE)"
        };
        let crate::Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
            panic!("table declaration")
        };
        let mut columns = table.columns;
        let mut constraints = TableConstraintSet {
            key_constraints: table.key_constraints,
            ..Default::default()
        };
        let occupied = ["t_a_not_null", "t_a_check", "t_a_key"]
            .map(str::to_owned)
            .into();
        let mut next = 0_u8;
        let result = crate::schema::constraint_metadata::materialize_constraint_metadata_with_names(
            &uqa_core::RelationIdentity::new("public", "t"),
            &mut columns,
            &mut constraints,
            &mut |_: &str| {
                next += 1;
                Ok([next; 16])
            },
            &occupied,
        );
        if explicit {
            let crate::schema::constraint_metadata::ConstraintMetadataError::Execution(error) =
                result.unwrap_err()
            else {
                panic!("expected SQL collision")
            };
            assert_eq!(error.sqlstate(), Some("42710"));
        } else {
            result.unwrap();
            let names = ConstraintNames::from_definition(&columns, &constraints)
                .entries()
                .map(|entry| entry.name)
                .collect::<Vec<_>>();
            assert_eq!(names, ["t_a_not_null1", "t_a_check1", "t_a_key1"]);
        }
    }
}
