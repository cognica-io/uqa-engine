//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::TableConstraintSet;
use uqa_core::RelationIdentity;

fn declaration() -> (Vec<ColumnDef>, TableConstraintSet) {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE p(v integer CONSTRAINT required NOT NULL, w integer CONSTRAINT other NOT NULL)").unwrap().remove(0) else { panic!("table declaration") };
    (table.columns, TableConstraintSet::default())
}

#[test]
fn legacy_not_null_migration_preserves_public_oids_and_runs_once() {
    let (mut columns, mut constraints) = declaration();
    let relation = RelationIdentity::new("public", "p");
    let mut next = 0_u8;
    let mut allocate = |_: &str| {
        next += 1;
        Ok([next; 16])
    };
    assert!(
        migrate_constraint_metadata(&relation, &mut columns, &mut constraints, &mut allocate)
            .unwrap()
    );
    let identities: Vec<_> = columns
        .iter()
        .map(|column| column.not_null_identity.unwrap())
        .collect();
    for (identity, name) in identities.iter().zip(["required", "other"]) {
        assert_eq!(
            identity.oid,
            crate::catalog::oids::stable_oid("constraint", &format!("public.p.{name}"))
        );
    }
    assert_ne!(identities[0].object_id, identities[1].object_id);
    assert!(
        !migrate_constraint_metadata(&relation, &mut columns, &mut constraints, &mut allocate)
            .unwrap()
    );
    columns[0].name = "renamed_column".into();
    columns[0].not_null_name = Some("renamed_constraint".into());
    assert!(!super::super::materialize_constraint_metadata(
        &RelationIdentity::new("moved", "renamed_table"),
        &mut columns,
        &mut constraints,
        &mut allocate
    )
    .unwrap());
    assert_eq!(
        columns
            .iter()
            .map(|column| column.not_null_identity.unwrap())
            .collect::<Vec<_>>(),
        identities
    );
}

#[test]
fn recreating_a_not_null_constraint_allocates_a_new_lifetime() {
    let (mut columns, mut constraints) = declaration();
    let relation = RelationIdentity::new("public", "p");
    let mut next = 0_u8;
    let mut allocate = |_: &str| {
        next += 1;
        Ok([next; 16])
    };
    super::super::materialize_constraint_metadata(
        &relation,
        &mut columns,
        &mut constraints,
        &mut allocate,
    )
    .unwrap();
    let before = columns[0].not_null_identity.unwrap();
    crate::schema::columns::publication::set_not_null(&mut columns, "p", "v", false).unwrap();
    assert!(columns[0].not_null_identity.is_none());
    crate::schema::columns::publication::set_not_null(&mut columns, "p", "v", true).unwrap();
    columns[0].not_null_name = Some("required".into());
    super::super::materialize_constraint_metadata(
        &relation,
        &mut columns,
        &mut constraints,
        &mut allocate,
    )
    .unwrap();
    let after = columns[0].not_null_identity.unwrap();
    assert_ne!(before.object_id, after.object_id);
    assert_ne!(before.oid, after.oid);
}

#[test]
fn missing_malformed_duplicate_and_retired_identities_are_rejected() {
    let (mut columns, _) = declaration();
    assert!(validate_not_null_identities(&columns).is_err());
    for (column, oid) in columns.iter_mut().zip([21_000, 21_001]) {
        column.not_null_identity = Some(ConstraintCatalogIdentity {
            object_id: [u8::try_from(oid - 20_999).unwrap(); 16],
            oid,
        });
    }
    validate_not_null_identities(&columns).unwrap();
    for identity in [
        ConstraintCatalogIdentity {
            object_id: [0; 16],
            oid: 21_000,
        },
        ConstraintCatalogIdentity {
            object_id: [1; 16],
            oid: 0,
        },
        ConstraintCatalogIdentity {
            object_id: [1; 16],
            oid: -1,
        },
        ConstraintCatalogIdentity {
            object_id: [1; 16],
            oid: i64::from(u32::MAX) + 1,
        },
        columns[1].not_null_identity.unwrap(),
    ] {
        let mut invalid = columns.clone();
        invalid[0].not_null_identity = Some(identity);
        assert!(validate_not_null_identities(&invalid).is_err());
    }
    columns[0].not_null = false;
    assert!(validate_not_null_identities(&columns).is_err());
}
