//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{Expr, Statement};

fn definition(declaration: &str) -> (Vec<ColumnDef>, Vec<TableCheck>) {
    let Statement::CreateTable(table) =
        crate::compile(&format!("CREATE TABLE fixture ({declaration})"))
            .unwrap()
            .remove(0)
    else {
        panic!("expected table")
    };
    (table.columns, table.checks)
}

#[test]
fn clear_default_changes_only_the_selected_column_and_is_idempotent() {
    let (mut columns, _) = definition("id integer DEFAULT 1, other integer DEFAULT 2");
    let other = serde_json::to_string(&columns[1]).unwrap();
    assert!(clear_foreign_column_default(&mut columns, "id"));
    assert!(columns[0].default.is_none());
    assert_eq!(serde_json::to_string(&columns[1]).unwrap(), other);
    assert!(!clear_foreign_column_default(&mut columns, "id"));
    assert!(!clear_foreign_column_default(&mut columns, "absent"));
}

#[test]
fn named_column_check_precedes_table_check_and_resets_all_check_metadata() {
    let (mut columns, mut checks) = definition(
        "id integer CONSTRAINT local_check CHECK (id > 0), CONSTRAINT table_check CHECK (id < 10)",
    );
    columns[0].check_name = Some("shared".into());
    columns[0].check_object_id = Some([7; 16]);
    columns[0].check_is_local = false;
    columns[0].check_enforced = false;
    columns[0].check_validated = false;
    columns[0].check_no_inherit = true;
    checks[0].name = Some("shared".into());
    let table_before = serde_json::to_string(&checks).unwrap();
    assert!(remove_foreign_check(&mut columns, &mut checks, "shared"));
    let column = &columns[0];
    assert!(
        column.check.is_none() && column.check_name.is_none() && column.check_object_id.is_none()
    );
    assert!(
        column.check_is_local
            && column.check_enforced
            && column.check_validated
            && !column.check_no_inherit
    );
    assert_eq!(serde_json::to_string(&checks).unwrap(), table_before);
    assert!(remove_foreign_check(&mut columns, &mut checks, "shared"));
    assert!(checks.is_empty());
    assert!(!remove_foreign_check(&mut columns, &mut checks, "shared"));
}

#[test]
fn unknown_check_name_preserves_unnamed_and_other_checks() {
    let (mut columns, mut checks) =
        definition("id integer CHECK (id > 0), CONSTRAINT keep CHECK (id < 10)");
    let before = serde_json::to_string(&(&columns, &checks)).unwrap();
    assert!(!remove_foreign_check(&mut columns, &mut checks, "absent"));
    assert_eq!(serde_json::to_string(&(&columns, &checks)).unwrap(), before);
}

#[test]
fn column_dependency_validation_rejects_defaults_and_generation_without_mutation() {
    let (mut columns, _) =
        definition("id integer, dependent integer GENERATED ALWAYS AS (id + 1) STORED");
    for default in [false, true] {
        if default {
            columns[1].generated = None;
            columns[1].default = Some(Expr::Column("id".into()));
        }
        let before = serde_json::to_string(&columns).unwrap();
        let error = validate_foreign_column_removal(&columns, "public.fixture", "id").unwrap_err();
        assert_eq!(error,"cannot drop generated column `public.fixture`.`id` because column `dependent` depends on it");
        assert_eq!(serde_json::to_string(&columns).unwrap(), before);
    }
    columns[1].default = None;
    columns[1].check = Some(Expr::Column("id".into()));
    columns[0].default = Some(Expr::Column("id".into()));
    validate_foreign_column_removal(&columns, "public.fixture", "id").unwrap();
}

#[test]
fn column_removal_clears_dependent_checks_and_preserves_other_identities() {
    let (mut columns,mut checks)=definition("id integer, dependent integer CHECK (id > 0), kept integer CHECK (kept > 0), CONSTRAINT drop_check CHECK (id < 10), CONSTRAINT keep_check CHECK (kept < 10)");
    columns[1].object_id = Some([1; 16]);
    columns[1].check_object_id = Some([2; 16]);
    columns[2].object_id = Some([3; 16]);
    let kept = serde_json::to_string(&columns[2]).unwrap();
    let kept_check = serde_json::to_string(&checks[1]).unwrap();
    remove_foreign_column(&mut columns, &mut checks, 0, "id");
    assert_eq!(columns.len(), 2);
    assert_eq!(columns[0].object_id, Some([1; 16]));
    assert!(columns[0].check.is_none() && columns[0].check_object_id.is_none());
    assert_eq!(serde_json::to_string(&columns[1]).unwrap(), kept);
    assert_eq!(checks.len(), 1);
    assert_eq!(serde_json::to_string(&checks[0]).unwrap(), kept_check);
}

#[test]
fn quoted_column_names_remain_atomic_during_dependency_removal() {
    let (mut columns, mut checks) =
        definition("\"a.b\" integer, a integer, dependent integer CHECK (\"a.b\" > 0)");
    let before = serde_json::to_string(&columns[2]).unwrap();
    remove_foreign_column(&mut columns, &mut checks, 1, "a");
    assert_eq!(columns[0].name, "a.b");
    assert_eq!(serde_json::to_string(&columns[1]).unwrap(), before);
}

#[test]
fn sequence_provenance_detachment_uses_exact_bound_names_and_preserves_defaults() {
    let (mut columns, _) = definition("id serial, other serial");
    columns[0].auto_increment.as_mut().unwrap().sequence = Some("public.seq".into());
    columns[1].auto_increment.as_mut().unwrap().sequence = Some("seq".into());
    let mut expected = columns.clone();
    expected[0].auto_increment = None;
    assert!(detach_foreign_sequence_provenance(
        &mut columns,
        "public.seq"
    ));
    assert_eq!(
        serde_json::to_string(&columns).unwrap(),
        serde_json::to_string(&expected).unwrap()
    );
    assert!(!detach_foreign_sequence_provenance(
        &mut columns,
        "public.seq"
    ));
}

#[test]
fn unknown_sequence_preserves_unbound_generation_metadata() {
    let (mut columns, _) = definition("id serial, plain integer DEFAULT 7");
    let before = serde_json::to_string(&columns).unwrap();
    assert!(!detach_foreign_sequence_provenance(
        &mut columns,
        "public.absent"
    ));
    assert_eq!(serde_json::to_string(&columns).unwrap(), before);
}
