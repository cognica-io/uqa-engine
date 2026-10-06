//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::Statement;

struct Catalog;

impl crate::FunctionTypeResolver for Catalog {
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
impl crate::routines::RoutineResolution for Catalog {}
impl crate::plan::AggregateClassifier for Catalog {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}
impl crate::expr::EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
}
impl SchemaExpressionCatalog for Catalog {
    fn registered_runtime_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }
    fn schema_expression_columns(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        unreachable!()
    }
}

#[test]
fn immutable_builtin_generation_rejects_stable_implicit_argument_casts() {
    for (source_type, call, accepted) in [
        ("timestamp", "date_trunc('day',v,'UTC')", false),
        ("date", "date_trunc('day',v,'UTC')", false),
        ("timestamptz", "date_trunc('day',v,'UTC')", true),
        ("timestamp", "date_trunc('day',v)", true),
        (
            "timestamp",
            "date_trunc('day',timestamp '2024-01-02','UTC')",
            false,
        ),
        (
            "timestamp",
            "date_trunc('day','2024-01-02 00:00:00+00','UTC')",
            true,
        ),
        ("timestamp", "date_trunc('day',NULL,'UTC')", true),
    ] {
        let Statement::CreateTable(mut table) = crate::compile(&format!(
            "CREATE TABLE t(v {source_type},g timestamptz GENERATED ALWAYS AS ({call}) STORED)"
        ))
        .unwrap()
        .remove(0) else {
            panic!("table declaration")
        };
        let mut expression = table.columns[1].generated.take().unwrap().expression;
        let result = crate::schema::generated::typing::infer_generation_expression(
            &Catalog,
            &table.columns,
            &mut expression,
        );
        if accepted {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some("42P17"), "{call}");
            assert_eq!(error.to_string(), "generation expression is not immutable");
        }
    }
}
