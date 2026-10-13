//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A CHECK belongs to its referenced attributes, not its syntactic column position.

use super::*;
use crate::ast::Statement;

#[test]
fn dropping_the_written_column_preserves_independent_check_identity_and_flags() {
    for (condition, foreign) in ["false", "t > 0"]
        .into_iter()
        .flat_map(|condition| [false, true].map(|foreign| (condition, foreign)))
    {
        let Statement::CreateTable(mut table) = crate::compile(&format!(
            "CREATE TABLE retained(n integer CONSTRAINT retained_check CHECK({condition}),t integer)",
        ))
        .unwrap()
        .remove(0) else {
            panic!("CREATE TABLE");
        };
        let column = &mut table.columns[0];
        column.check_object_id = Some([7; 16]);
        column.check_catalog_oid = Some(90_001);
        column.check_enforced = false;
        column.check_validated = false;
        column.check_is_local = false;
        column.check_no_inherit = true;
        let original =
            crate::schema::constraint_changes::take_column_check(&mut column.clone()).unwrap();
        if foreign {
            crate::schema::foreign_tables::dependencies::remove_foreign_column(
                &mut table.columns,
                &mut table.checks,
                "n",
            );
        } else {
            table
                .checks
                .extend(remove_column_declarations(&mut table.columns, "n"));
            remove_column_checks(&mut table.checks, "n");
        }
        assert_eq!(table.columns.len(), 1);
        assert_eq!(table.columns[0].name, "t");
        assert_eq!(
            serde_json::to_value(&table.checks).unwrap(),
            serde_json::to_value(vec![original]).unwrap(),
            "{condition}, foreign={foreign}"
        );
        remove_column_checks(&mut table.checks, "t");
        assert_eq!(table.checks.is_empty(), condition == "t > 0");
    }
}
