//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generation expressions bind declared columns without looking up a CHECK whole-row type.

use super::*;
use crate::ast::{FunctionBinding, FunctionVolatility, Statement};
use crate::schema::{
    expressions::PlannedSchemaExpression, SchemaBindingContext, SchemaExpressionCatalog,
};
use crate::{expr::EngineHook, routines::RoutineResolution, FunctionTypeResolver};

struct Catalog;

impl FunctionTypeResolver for Catalog {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        panic!("generation columns do not require a relation type lookup: {name}");
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
        panic!("binding does not execute expressions");
    }

    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("binding does not execute expressions");
    }

    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("binding does not execute expressions");
    }
}

impl SchemaExpressionCatalog for Catalog {
    fn plan_schema_expression(
        &self,
        expression: &Expr,
        _: &[ColumnDef],
    ) -> Result<PlannedSchemaExpression, SQLError> {
        Ok(PlannedSchemaExpression {
            expression: expression.clone(),
            ty: Some(ColumnType::Integer),
            immutable: true,
            constant: false,
        })
    }

    fn registered_runtime_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }

    fn schema_expression_columns(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        panic!("generation binding uses the supplied declaration columns");
    }
}

#[test]
fn generated_columns_bind_their_declared_scope_without_relation_type_lookup() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    for sql in [
        "CREATE TABLE outside.sample(n int, g int GENERATED ALWAYS AS (n+1) STORED)",
        "CREATE TABLE \"Outer\".\"Mixed name\"(n int, g int GENERATED ALWAYS AS (\"Mixed name\".n+1) STORED)",
    ] {
        let Statement::CreateTable(mut table) = crate::compile(sql).unwrap().remove(0) else {
            panic!("CREATE TABLE fixture");
        };
        prepare_generated_columns(
            &context,
            &table.qualifier,
            &mut table.columns,
            &table.foreign_keys,
        )
        .unwrap();
        assert!(table.columns[1].generated.is_some());
    }
}
