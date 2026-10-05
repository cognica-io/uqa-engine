//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnType;

fn declaration(column: &str, name: Option<&str>) -> NotNullDeclaration {
    NotNullDeclaration {
        column: column.into(),
        name: name.map(str::to_string),
        no_inherit: false,
        explicit: true,
    }
}

#[test]
fn foreign_not_null_declarations_merge_and_materialize_the_recorded_constraint() {
    let relation = uqa_core::RelationIdentity::from_legacy_name("public.fixture").unwrap();
    let names = crate::schema::constraint_metadata::ConstraintNameScope::default();
    let mut columns = vec![
        ColumnDef::nullable("a", ColumnType::Integer),
        ColumnDef::nullable("b", ColumnType::Integer),
    ];
    let mut next = 0_u8;
    let mut allocate = |_: &str| {
        next += 1;
        Ok([next; 16])
    };
    define_foreign_not_null_constraints(
        ForeignNotNullContext {
            relation: &relation,
            relation_oid: 16_384,
            checks: &[],
            names: &names,
        },
        &mut columns,
        vec![declaration("a", None), declaration("a", Some("forced_nn"))],
        &mut allocate,
    )
    .unwrap();
    assert!(columns[0].not_null);
    assert!(columns[0].not_null_validated);
    assert!(columns[0].not_null_is_local);
    assert_eq!(columns[0].not_null_name.as_deref(), Some("forced_nn"));
    assert!(columns[0].not_null_identity.is_some());
    assert!(!columns[1].not_null);
    assert!(columns[1].not_null_identity.is_none());
    assert_eq!(next, 1);
}

#[test]
fn foreign_not_null_missing_column_does_not_materialize_any_constraint() {
    let relation = uqa_core::RelationIdentity::from_legacy_name("public.fixture").unwrap();
    let names = crate::schema::constraint_metadata::ConstraintNameScope::default();
    let mut columns = vec![ColumnDef::nullable("a", ColumnType::Integer)];
    let mut allocate =
        |_: &str| -> crate::schema::constraint_metadata::ConstraintMetadataResult<[u8; 16]> {
            panic!("missing-column validation must precede identity allocation");
        };
    let error = define_foreign_not_null_constraints(
        ForeignNotNullContext {
            relation: &relation,
            relation_oid: 16_384,
            checks: &[],
            names: &names,
        },
        &mut columns,
        vec![
            declaration("a", None),
            declaration("missing", Some("bad_nn")),
        ],
        &mut allocate,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"));
    assert_eq!(
        error.to_string(),
        "column \"missing\" of relation \"fixture\" does not exist"
    );
    assert!(!columns[0].not_null);
    assert!(columns[0].not_null_identity.is_none());
}
