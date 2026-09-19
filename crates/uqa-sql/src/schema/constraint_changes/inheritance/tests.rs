//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn declaration(sql: &str) -> (Vec<ColumnDef>, TableConstraintSet) {
    let crate::Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
        panic!("table declaration");
    };
    (
        table.columns,
        TableConstraintSet {
            checks: table.checks,
            foreign_keys: table.foreign_keys,
            key_constraints: table.key_constraints,
            ..TableConstraintSet::default()
        },
    )
}

#[test]
fn inherited_not_null_keys_follow_the_column_and_check_keys_follow_the_name() {
    let (columns, constraints) = declaration("CREATE TABLE child(unrelated integer CONSTRAINT root_nn NOT NULL, v integer CONSTRAINT child_nn NOT NULL CONSTRAINT positive CHECK(v>0), CONSTRAINT table_check CHECK(v<10))");
    let not_null = InheritedConstraintKey::NotNull("v")
        .require("child", &columns, &constraints)
        .unwrap();
    assert_eq!(not_null.name, "child_nn");
    assert_eq!(not_null.key, InheritedConstraintKey::NotNull("v"));
    for name in ["positive", "table_check"] {
        let check = InheritedConstraintKey::Check(name)
            .require("child", &columns, &constraints)
            .unwrap();
        assert_eq!(check.name, name);
        assert_eq!(check.key, InheritedConstraintKey::Check(name));
    }
    assert!(InheritedConstraintKey::Check("root_nn")
        .find(&columns, &constraints)
        .is_none());
    assert_eq!(
        InheritedConstraintKey::NotNull("missing")
            .require("child", &columns, &constraints)
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
    assert_eq!(
        InheritedConstraintKey::Check("missing")
            .require("child", &columns, &constraints)
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
}

#[test]
fn inherited_removal_preserves_local_or_remaining_parent_definitions() {
    use InheritedConstraintRemoval::{Drop, Keep, MakeLocal};
    for (recurse, local, parents, expected) in [
        (true, false, 0, Drop),
        (true, true, 0, Keep),
        (true, false, 1, Keep),
        (true, true, 1, Keep),
        (false, false, 0, MakeLocal),
        (false, true, 0, Keep),
        (false, false, 1, Keep),
        (false, true, 1, Keep),
    ] {
        assert_eq!(
            inherited_constraint_removal(recurse, local, parents),
            expected
        );
    }
}

#[test]
fn making_an_inherited_constraint_local_changes_only_its_origin() {
    let (mut columns, mut constraints) = declaration("CREATE TABLE child(v integer CONSTRAINT nn NOT NULL CONSTRAINT positive CHECK(v>0), CONSTRAINT table_check CHECK(v<10))");
    columns[0].not_null_is_local = false;
    columns[0].check_is_local = false;
    constraints.checks[0].is_local = false;
    for name in ["nn", "positive", "table_check"] {
        assert!(
            !InheritedConstraint::find(&columns, &constraints, name)
                .unwrap()
                .is_local
        );
        make_constraint_local(&mut columns, &mut constraints, name).unwrap();
        assert!(
            InheritedConstraint::find(&columns, &constraints, name)
                .unwrap()
                .is_local
        );
    }
    assert!(columns[0].not_null);
    assert!(columns[0].check.is_some());
    assert_eq!(constraints.checks.len(), 1);
}

#[test]
fn noninherited_constraints_preserve_their_scope_and_other_kinds_do_not_recurse() {
    let (columns, constraints) = declaration("CREATE TABLE t(v integer CONSTRAINT nn NOT NULL NO INHERIT CONSTRAINT positive CHECK(v>0) NO INHERIT REFERENCES p(id), CONSTRAINT key_name UNIQUE(v))");
    for name in ["nn", "positive"] {
        let target = InheritedConstraint::find(&columns, &constraints, name).unwrap();
        assert!(target.no_inherit);
        assert!(target.is_local);
    }
    assert!(InheritedConstraint::find(&columns, &constraints, "key_name").is_none());
    assert!(InheritedConstraint::find(&columns, &constraints, "missing").is_none());
    ensure_inherited_constraint_removable("public.t", "nn", 0).unwrap();
    let error = ensure_inherited_constraint_removable("public.t", "nn", 1).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P16"));
    assert!(error.to_string().contains("relation \"t\""));
}
