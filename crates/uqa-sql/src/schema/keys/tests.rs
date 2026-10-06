//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{AlterTableAction, Statement},
    compiler::compile,
};

#[test]
fn alter_key_repetition_is_checked_after_binding_and_before_naming_the_index() {
    let Some(Statement::CreateTable(table)) = compile("CREATE TABLE t (a integer)").unwrap().pop()
    else {
        panic!("expected CREATE TABLE");
    };
    let relation = AddedKeyRelation {
        table: "public.t",
        columns: &table.columns,
        keys: &[],
        partition: None,
    };
    for (clause, message) in [
        (
            "UNIQUE (a,a)",
            "column \"a\" appears twice in unique constraint",
        ),
        (
            "PRIMARY KEY (a,a)",
            "column \"a\" appears twice in primary key constraint",
        ),
    ] {
        let Some(Statement::AlterTable(mut alter)) =
            compile(&format!("ALTER TABLE t ADD {clause}"))
                .unwrap()
                .pop()
        else {
            panic!("expected ALTER TABLE");
        };
        let Some(AlterTableAction::AddKeyConstraint { constraint }) = alter.actions.pop() else {
            panic!("expected ADD key");
        };
        assert!(constraint.name.is_none());
        assert!(constraint.catalog_identity.is_none());
        assert!(constraint.index_identity.is_none());
        let error = validate_added_key(&relation, &constraint).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42701"));
        assert_eq!(error.to_string(), message);
    }
}
