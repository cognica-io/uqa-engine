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

#[test]
fn primary_key_not_null_preparation_matches_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/primary_key_not_null_oracle.expected.json"
    )))
    .unwrap();
    let Statement::CreateTable(mut table) =
        compile("CREATE TABLE t(a integer CONSTRAINT owned_nn NOT NULL)")
            .unwrap()
            .remove(0)
    else {
        panic!("expected CREATE TABLE");
    };
    for (case, relation, name, no_inherit, validated) in [
        (
            "alter_plain",
            "not_null_primary_probe.t",
            "owned_nn",
            true,
            true,
        ),
        (
            "alter_repeated_column",
            "not_null_primary_probe.t",
            "owned_nn",
            true,
            true,
        ),
        (
            "alter_missing_after",
            "not_null_primary_probe.t",
            "owned_nn",
            true,
            true,
        ),
        (
            "alter_missing_before",
            "not_null_primary_probe.t",
            "owned_nn",
            true,
            true,
        ),
        (
            "invalid_not_valid",
            "not_null_validation_probe.t",
            "pending_nn",
            false,
            false,
        ),
        (
            "invalid_both_flags",
            "not_null_validation_probe.u",
            "pending_nn",
            true,
            false,
        ),
    ] {
        let expected = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["id"] == case)
            .unwrap();
        let Statement::AlterTable(mut alter) = compile(expected["sql"].as_str().unwrap())
            .unwrap()
            .remove(0)
        else {
            panic!("expected ALTER TABLE");
        };
        let AlterTableAction::AddKeyConstraint { mut constraint } = alter.actions.remove(0) else {
            panic!("expected key action");
        };
        let column = &mut table.columns[0];
        column.not_null_name = Some(name.into());
        column.not_null_no_inherit = no_inherit;
        column.not_null_validated = validated;
        let error =
            validate_primary_key_not_null_constraints(relation, &table.columns, &constraint)
                .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            expected["error"]["sqlstate"].as_str(),
            "{case}"
        );
        assert_eq!(error.to_string(), expected["error"]["message"], "{case}");
        assert_eq!(
            error.detail(),
            expected["error"]["detail"].as_str(),
            "{case}"
        );
        assert_eq!(error.hint(), expected["error"]["hint"].as_str(), "{case}");
        constraint.kind = TableKeyConstraintKind::Unique;
        validate_primary_key_not_null_constraints(relation, &table.columns, &constraint).unwrap();
        constraint.kind = TableKeyConstraintKind::PrimaryKey;
        table.columns[0].not_null_no_inherit = false;
        table.columns[0].not_null_validated = true;
        validate_primary_key_not_null_constraints(relation, &table.columns, &constraint).unwrap();
    }
}
