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
