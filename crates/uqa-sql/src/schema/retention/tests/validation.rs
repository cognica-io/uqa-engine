//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionVolatility, schema::SchemaExpressionCatalog, SQLError};

struct Catalog;

impl crate::FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> std::result::Result<Option<ColumnType>, SQLError> {
        unreachable!("subqueries are rejected before resolving function types")
    }
}

impl crate::routines::RoutineResolution for Catalog {}

impl crate::plan::AggregateClassifier for Catalog {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

impl crate::expr::EngineHook for Catalog {
    fn nextval(&self, _: &str) -> std::result::Result<i64, SQLError> {
        unreachable!("column validation does not execute sequences")
    }
    fn currval(&self, _: &str) -> std::result::Result<i64, SQLError> {
        unreachable!("column validation does not execute sequences")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> std::result::Result<i64, SQLError> {
        unreachable!("column validation does not execute sequences")
    }
}

impl SchemaExpressionCatalog for Catalog {
    fn registered_runtime_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        (name == "generated_twice").then_some(FunctionVolatility::Immutable)
    }
    fn schema_expression_columns(
        &self,
        _: &str,
    ) -> std::result::Result<Option<Vec<ColumnDef>>, SQLError> {
        unreachable!("subqueries are rejected before resolving catalog columns")
    }
}

#[test]
fn virtual_generated_calls_preserve_postgresql_error_fields() {
    for expression in ["generated_twice(v)", "coalesce(generated_twice(v), 0)"] {
        for kind in ["", " VIRTUAL"] {
            let mut source = columns(&format!(
                "CREATE TABLE t(v integer, g integer GENERATED ALWAYS AS ({expression}){kind})"
            ));
            let error = crate::schema::generated::prepare_generated_columns(
                &Catalog,
                "t",
                &mut source,
                &[],
                &[],
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"));
            assert_eq!(
                error.to_string(),
                "generation expression uses user-defined function"
            );
            let SQLError::Diagnostic { detail, hint, .. } = error else {
                panic!("the PostgreSQL detail must remain separate from the primary message");
            };
            assert_eq!(detail.as_deref(), Some("Virtual generated columns that make use of user-defined functions are not yet supported."));
            assert_eq!(hint, None);
        }
    }
}

#[test]
fn generated_column_owner_rejects_the_query_shapes_excluded_from_retention() {
    for sql in [
        "SELECT (SELECT 1)",
        "SELECT EXISTS (SELECT 1)",
        "SELECT 1 IN (SELECT 1)",
    ] {
        let Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
            panic!("expected a SELECT");
        };
        let mut source = columns("CREATE TABLE t(v integer)");
        source[0].generated = Some(GeneratedColumn {
            kind: GeneratedColumnKind::Stored,
            expression: Box::new(query.projections.remove(0).expr),
            function_dependencies: Vec::new(),
        });
        let error = crate::schema::generated::prepare_generated_columns(
            &Catalog,
            "t",
            &mut source,
            &[],
            &[],
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("cannot use subquery"),
            "{sql}: {error}"
        );
    }
}
