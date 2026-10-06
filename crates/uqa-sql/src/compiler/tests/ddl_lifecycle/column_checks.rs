//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn multiple_column_checks_preserve_every_name_expression_and_attribute() {
    let declaration = "v int CONSTRAINT upper CHECK(v<10) NO INHERIT CONSTRAINT positive CHECK(v>0) NOT ENFORCED CONSTRAINT nonzero CHECK(v<>0) ENFORCED";
    for sql in [
        format!("CREATE TABLE t({declaration})"),
        format!("ALTER TABLE t ADD COLUMN IF NOT EXISTS {declaration}"),
    ] {
        let (column, checks) = match first(&sql) {
            Statement::CreateTable(mut table) => (table.columns.remove(0), table.checks),
            Statement::AlterTable(mut alter) => {
                let AlterTableAction::AddColumn {
                    column,
                    checks,
                    if_not_exists,
                    ..
                } = alter.actions.remove(0)
                else {
                    panic!("expected ADD COLUMN");
                };
                assert!(if_not_exists);
                (column, checks)
            }
            other => panic!("unexpected declaration: {other:?}"),
        };
        assert!(column.check.is_none());
        assert_eq!(checks.len(), 3);
        for (check, (name, expression, enforced, no_inherit)) in checks.iter().zip([
            ("upper", "v<10", true, true),
            ("positive", "v>0", false, false),
            ("nonzero", "v<>0", true, false),
        ]) {
            assert_eq!(check.name.as_deref(), Some(name));
            let Statement::CreateTable(expected) =
                first(&format!("CREATE TABLE expected(v int CHECK({expression}))"))
            else {
                panic!("expected CHECK declaration")
            };
            assert_eq!(Some(&check.expr), expected.columns[0].check.as_ref());
            assert_eq!(check.enforced, enforced);
            assert_eq!(check.validated, enforced);
            assert_eq!(check.no_inherit, no_inherit);
            assert!(check.is_local);
        }
    }
}

#[test]
fn foreign_keys_preserve_column_and_table_order_and_repeated_clauses() {
    use crate::ast::DeclaredForeignKey::{Column, Table};
    let Statement::CreateTable(table) = first("CREATE TABLE t(a int, CONSTRAINT first FOREIGN KEY(a) REFERENCES p, b int REFERENCES q, c int CONSTRAINT third REFERENCES p CONSTRAINT fourth REFERENCES q DEFERRABLE INITIALLY DEFERRED, FOREIGN KEY(c) REFERENCES r)") else {
        panic!("expected CREATE TABLE")
    };
    assert_eq!(
        table.foreign_key_order,
        vec![Table(0), Column("b".into()), Table(1), Table(2), Table(3)]
    );
    assert!(table.columns[2].references.is_none());
    assert_eq!(table.foreign_keys[1].name.as_deref(), Some("third"));
    assert!(!table.foreign_keys[1].deferrable);
    assert_eq!(table.foreign_keys[2].name.as_deref(), Some("fourth"));
    assert!(table.foreign_keys[2].deferrable);
    assert!(table.foreign_keys[2].initially_deferred);
}

#[test]
fn added_columns_retain_every_foreign_key_and_its_own_attributes() {
    let Statement::AlterTable(mut table) = first("ALTER TABLE t ADD COLUMN a int CONSTRAINT first REFERENCES p NOT ENFORCED CONSTRAINT second REFERENCES q DEFERRABLE") else {
        panic!("expected ALTER TABLE")
    };
    let AlterTableAction::AddColumn {
        column,
        foreign_keys,
        ..
    } = table.actions.remove(0)
    else {
        panic!("expected ADD COLUMN")
    };
    assert!(column.references.is_none());
    assert_eq!(foreign_keys.len(), 2);
    assert_eq!(foreign_keys[0].name.as_deref(), Some("first"));
    assert!(!foreign_keys[0].enforced);
    assert!(!foreign_keys[0].deferrable);
    assert_eq!(foreign_keys[1].name.as_deref(), Some("second"));
    assert!(foreign_keys[1].enforced);
    assert!(foreign_keys[1].deferrable);
}
