//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn declaration(sql: &str) -> (Vec<ColumnDef>, TableConstraintSet) {
    let crate::Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected table declaration");
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
fn inherited_not_null_validation_follows_the_column_instead_of_the_constraint_name() {
    let (mut columns, constraints) =
        declaration("CREATE TABLE t(v integer CONSTRAINT root_nn NOT NULL)");
    columns[0].not_null_validated = false;
    let target = constraint_validation("t", "root_nn", &columns, &constraints).unwrap();
    assert!(target.requires_descendants());
    let (children, _) = declaration("CREATE TABLE child(unrelated integer CONSTRAINT root_nn NOT NULL, v integer CONSTRAINT child_nn NOT NULL)");
    assert_eq!(
        target
            .child_constraint_name("root_nn", "child", &children)
            .unwrap(),
        "child_nn"
    );
    assert_eq!(
        target
            .child_constraint_name("root_nn", "child", &children[..1])
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
}

#[test]
fn validated_and_noninherited_constraints_do_not_prepare_descendants() {
    let (mut columns, mut constraints) = declaration("CREATE TABLE t(v integer CONSTRAINT nn NOT NULL CONSTRAINT inline_check CHECK(v>0), CONSTRAINT table_check CHECK(v<10))");
    for validated in [false, true] {
        for no_inherit in [false, true] {
            columns[0].not_null_validated = validated;
            columns[0].not_null_no_inherit = no_inherit;
            columns[0].check_validated = validated;
            columns[0].check_no_inherit = no_inherit;
            constraints.checks[0].validated = validated;
            constraints.checks[0].no_inherit = no_inherit;
            for name in ["nn", "inline_check", "table_check"] {
                let target = constraint_validation("t", name, &columns, &constraints).unwrap();
                assert_eq!(
                    target.requires_descendants(),
                    !validated && !no_inherit,
                    "{name}"
                );
            }
        }
    }
}

#[test]
fn foreign_key_dependencies_preserve_the_reference_and_check_enforcement_before_validation() {
    let (mut columns, mut constraints) = declaration("CREATE TABLE t(v integer CONSTRAINT inline_fk REFERENCES p(id), CONSTRAINT table_fk FOREIGN KEY(v) REFERENCES p(id))");
    columns[0].references.as_mut().unwrap().validated = false;
    constraints.foreign_keys[0].validated = false;
    for name in ["inline_fk", "table_fk"] {
        let target = constraint_validation("t", name, &columns, &constraints).unwrap();
        assert_eq!(
            target.kind,
            ConstraintValidationKind::ForeignKey {
                referenced_table: "p"
            }
        );
        assert!(!target.validated);
        assert!(!target.requires_descendants());
    }
    let inline = columns[0].references.as_mut().unwrap();
    inline.enforced = false;
    inline.validated = true;
    constraints.foreign_keys[0].enforced = false;
    constraints.foreign_keys[0].validated = true;
    for name in ["inline_fk", "table_fk"] {
        assert_eq!(
            constraint_validation("t", name, &columns, &constraints)
                .unwrap_err()
                .sqlstate(),
            Some("55000")
        );
    }
}

#[test]
fn validation_rejects_missing_or_key_constraints_and_only_recursion() {
    let (columns, constraints) =
        declaration("CREATE TABLE t(v integer, CONSTRAINT key_name PRIMARY KEY(v))");
    for (name, state) in [("missing", "42704"), ("key_name", "42809")] {
        assert_eq!(
            constraint_validation("t", name, &columns, &constraints)
                .unwrap_err()
                .sqlstate(),
            Some(state)
        );
    }
    assert_eq!(
        ensure_validation_recurses(false, true)
            .unwrap_err()
            .sqlstate(),
        Some("42P16")
    );
    ensure_validation_recurses(false, false).unwrap();
    ensure_validation_recurses(true, true).unwrap();
}
