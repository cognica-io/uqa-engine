//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 lifecycle reference: raw SHA-256 d5251bbdb436017eaa70edc19728d05a065346fd57907d0e1ebf0531a8a7af76, pinned image bc8a94e4a938576533c0f0e8ad6f2e8c67d5c5facef05005a5e5cf9adce2fe25.

use super::*;
use crate::schema::{
    check_inheritance::bind_parent_check_columns,
    columns::removal_metadata::{remove_column_checks, remove_column_declarations},
    dependencies::{rewrites::*, schema_expr_depends_on_column, schema_expr_references_column},
};

pub(super) fn analyzed(sql: &str) -> crate::ast::CreateTable {
    let Statement::CreateTable(mut table) = crate::compile(sql).unwrap().remove(0) else {
        panic!("CREATE TABLE");
    };
    let columns = table.columns.clone();
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    for expression in table
        .columns
        .iter_mut()
        .filter_map(|column| column.check.as_mut())
        .chain(table.checks.iter_mut().map(|check| &mut check.expr))
    {
        validate_check_expression_with_type(
            &context,
            &table.name,
            &table.qualifier,
            &columns,
            &row_type(&table.name),
            expression,
        )
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    table
}

#[test]
fn whole_row_checks_keep_relation_dependencies_when_columns_change() {
    let mut table = analyzed(
        "CREATE TABLE whole(n integer,t text,CONSTRAINT keep_all CHECK(whole IS NOT NULL),CONSTRAINT n_positive CHECK((whole).n>0))",
    );
    let whole = &table.checks[0].expr;
    assert!(schema_expr_references_column(whole, "n"));
    assert!(!schema_expr_depends_on_column(whole, "n"));
    assert!(schema_expr_depends_on_column(&table.checks[1].expr, "n"));
    assert!(!schema_expr_depends_on_column(&table.checks[1].expr, "t"));
    let before = whole.clone();
    for check in &mut table.checks {
        rename_schema_expr_column(&mut check.expr, "n", "renamed").unwrap();
    }
    assert_eq!(table.checks[0].expr, before);
    assert!(schema_expr_depends_on_column(
        &table.checks[1].expr,
        "renamed"
    ));
    assert!(!schema_expr_depends_on_column(&table.checks[1].expr, "n"));
    remove_column_checks(&mut table.checks, "renamed");
    assert_eq!(table.checks.len(), 1);
    assert_eq!(table.checks[0].name.as_deref(), Some("keep_all"));
    let original = crate::RelationIdentity::new("public", "whole");
    assert!(schema_expr_references_relation(
        &table.checks[0].expr,
        &original
    ));
    rename_schema_expr_relation(&mut table.checks[0].expr, &original, "moved.renamed").unwrap();
    assert!(!schema_expr_references_relation(
        &table.checks[0].expr,
        &original
    ));
    assert!(schema_expr_references_relation(
        &table.checks[0].expr,
        &crate::RelationIdentity::new("moved", "renamed")
    ));
}

#[test]
fn a_surviving_column_check_moves_to_the_table_with_its_original_identity() {
    // The separate constant CHECK observation uses raw capture 532994365b3283f75dbe706ecb497290a3bedfe2abb37fc5f98fdcfee872805a.
    for (condition, foreign) in ["column_whole IS NOT NULL", "true"]
        .into_iter()
        .flat_map(|condition| [false, true].map(|foreign| (condition, foreign)))
    {
        let mut table = analyzed(&format!(
            "CREATE TABLE column_whole(n integer CONSTRAINT all_values CHECK({condition}),t text)",
        ));
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
    }
}

#[test]
fn inheritance_rejects_whole_rows_but_preserves_explicit_field_checks() {
    for (table, name, enforced) in [
        ("whole", "keep_all", true),
        ("ignored", "ignored_whole", false),
    ] {
        let mut declaration = analyzed(&format!(
            "CREATE TABLE {table}(n integer,CONSTRAINT {name} CHECK({table} IS NOT NULL))"
        ));
        declaration.checks[0].enforced = enforced;
        let error = bind_parent_check_columns(
            &format!("check_lifecycle.{table}"),
            &mut declaration.checks[0],
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert_eq!(
            error.to_string(),
            "cannot convert whole-row table reference"
        );
        assert_eq!(
            error.detail(),
            Some(
                format!(
                    "Constraint \"{name}\" contains a whole-row reference to table \"{table}\"."
                )
                .as_str()
            )
        );
        assert_eq!(error.hint(), None);
    }
    let mut declaration =
        analyzed("CREATE TABLE fields(n integer,t text,CONSTRAINT n_positive CHECK((fields).n>0))");
    let original = declaration.checks[0].expr.clone();
    bind_parent_check_columns("check_lifecycle.fields", &mut declaration.checks[0]).unwrap();
    assert_eq!(declaration.checks[0].expr, original);
    assert!(schema_expr_depends_on_column(&original, "n"));
    assert!(!schema_expr_depends_on_column(&original, "t"));
}
