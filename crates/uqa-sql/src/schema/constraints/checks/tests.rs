//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FunctionBinding, FunctionVolatility, Statement};
use crate::schema::{ScalarTypeSchema, SchemaExpressionCatalog};
use crate::{expr::EngineHook, routines::RoutineResolution, FunctionTypeResolver};

mod catalog_references;
mod foreign;
mod lifecycle;

struct Catalog;

impl FunctionTypeResolver for Catalog {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(matches!(name, "foreign_named" | "public.foreign_named").then(|| row_type(name)))
    }

    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl RoutineResolution for Catalog {}

impl crate::plan::AggregateClassifier for Catalog {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("CHECK analysis must not execute expressions");
    }

    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("CHECK analysis must not execute expressions");
    }

    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("CHECK analysis must not execute expressions");
    }
}

impl SchemaExpressionCatalog for Catalog {
    fn plan_schema_expression(
        &self,
        _: &Expr,
        _: &[ColumnDef],
    ) -> Result<crate::schema::expressions::PlannedSchemaExpression, SQLError> {
        panic!("CHECK analysis must not optimize expressions");
    }

    fn registered_runtime_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }

    fn schema_expression_columns(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        panic!("the declaring relation supplies its own columns");
    }
}

fn row_type(table: &str) -> ColumnType {
    crate::catalog::relation_oids::RelationCatalogOids {
        relation: 80_001,
        row_type: Some(80_002),
        array_type: Some(80_003),
        rule: None,
    }
    .row_type_reference(&crate::RelationIdentity::from_legacy_name(table).unwrap())
    .map(ColumnType::Composite)
    .unwrap()
}

#[test]
fn table_check_analysis_matches_postgresql_row_scope_and_error_order() {
    // PostgreSQL 18.4, image bc8a94e4a938576533c0f0e8ad6f2e8c67d5c5facef05005a5e5cf9adce2fe25; raw capture SHA-256 64422390d3cf735d89ba7bee8812148aff04001c2b1f97946578fda7bf1f501a.
    assert_table_check_analysis(include_str!("tests/analysis.json"), 23);
}

#[test]
fn whole_row_field_diagnostics_follow_postgresql_column_analysis_order() {
    // Same pinned PostgreSQL 18.4 image; raw capture SHA-256 532994365b3283f75dbe706ecb497290a3bedfe2abb37fc5f98fdcfee872805a.
    assert_table_check_analysis(include_str!("tests/field_scope.json"), 5);
}

fn assert_table_check_analysis(source: &str, expected_cases: usize) {
    let fixture: serde_json::Value = serde_json::from_str(source).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), expected_cases);
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    for case in cases {
        let sql = case["sql"].as_str().unwrap();
        let Statement::CreateTable(mut table) = crate::compile(sql).unwrap().remove(0) else {
            panic!("CREATE TABLE fixture: {sql}");
        };
        assert_eq!(table.checks.len(), 1, "{sql}");
        let result = validate_check_expression_with_type(
            &context,
            &table.name,
            &table.qualifier,
            &table.columns,
            &row_type(&table.name),
            &mut table.checks[0].expr,
        );
        if case["error"].is_null() {
            result.unwrap_or_else(|error| panic!("{sql}: {error}"));
        } else {
            let error = result.expect_err(sql);
            assert_eq!(
                error.sqlstate(),
                case["error"]["sqlstate"].as_str(),
                "{sql}"
            );
            assert_eq!(
                error.to_string(),
                case["error"]["message"].as_str().unwrap(),
                "{sql}"
            );
        }
    }
}

#[test]
fn check_row_scope_retains_real_identity_without_exposing_system_attributes_as_fields() {
    let Statement::CreateTable(table) = crate::compile("CREATE TABLE scoped(n integer)")
        .unwrap()
        .remove(0)
    else {
        panic!("CREATE TABLE");
    };
    let row_type = row_type(&table.name);
    let schema = check_row_schema("scoped", &table.columns, &row_type).unwrap();
    assert_eq!(schema.whole_row_type("scoped"), Some(&row_type));
    assert_eq!(schema.columns().len(), 1);
    assert_eq!(schema.type_of("tableoid"), Some(&ColumnType::Oid));
    assert_eq!(schema.type_of("ctid"), None);
    let empty = check_row_schema("scoped", &[], &row_type).unwrap();
    assert!(empty.has_qualifier("scoped"));
    assert!(empty.columns().is_empty());
    assert_eq!(empty.whole_row_type("scoped"), Some(&row_type));
}
