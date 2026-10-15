//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{CompositeTypeReference, FunctionBinding, Statement};

struct Catalog;

impl FunctionTypeResolver for Catalog {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        assert_eq!(name, "public.scoped");
        Ok(Some(ColumnType::Composite(CompositeTypeReference {
            schema: "public".into(),
            name: "scoped".into(),
            oid: 80_002,
            array_oid: 80_003,
            relation_oid: 80_001,
        })))
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

struct Expressions;

impl PartitionExpressions for Expressions {
    fn expression_text(&self, _: &Expr) -> Result<String, SQLError> {
        panic!("row evaluation does not render SQL");
    }

    fn evaluate_bound(&self, _: &Expr, _: &[SQLParam]) -> Result<Value, SQLError> {
        panic!("CHECK evaluation does not evaluate a partition bound");
    }

    fn evaluate_row(
        &self,
        expression: &Expr,
        row: &dyn RowLookup,
        schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        crate::scalar::plan::eval_physical(
            &uqa_sql::plan::ExpressionPlan::lower(expression.clone()),
            &crate::scalar::plan::PhysicalEvalContext::from_row_lookup(row, params)
                .with_row_schema(schema),
        )
    }
}

#[test]
fn check_rows_borrow_fields_and_use_the_physical_relation_oid() {
    let Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE scoped(n integer,t text,CHECK(scoped.* IS NOT NULL))")
            .unwrap()
            .remove(0)
    else {
        panic!("CREATE TABLE");
    };
    let scope = CheckRowScope::new(&Catalog, "public.scoped", &table.columns).unwrap();
    let row = ResultRow::from([
        ("n".into(), Value::Int(1)),
        ("t".into(), Value::Str("borrowed payload".into())),
        ("tableoid".into(), Value::Int(7)),
        ("unrelated".into(), Value::Null),
    ]);
    let borrowed = CheckRow {
        scope: &scope,
        row: &row,
    };
    assert!(std::ptr::eq(
        borrowed.column("t").unwrap(),
        row.get("t").unwrap()
    ));
    for expression in [
        Expr::Column("tableoid".into()),
        Expr::QualifiedColumn {
            qualifier: "scoped".into(),
            column: "tableoid".into(),
        },
    ] {
        assert_eq!(
            scope
                .evaluate(&Expressions, &expression, &row, &[])
                .unwrap(),
            Value::Int(80_001)
        );
    }
    assert!(borrowed.qualified_column("other", "tableoid").is_none());
    assert!(borrowed.column("unrelated").is_none());
    assert_eq!(scope.schema.columns(), &["n", "t"]);
    assert_eq!(
        scope
            .evaluate(&Expressions, &table.checks[0].expr, &row, &[])
            .unwrap(),
        Value::Bool(true)
    );
    for null_column in ["n", "t"] {
        for explicit_null in [false, true] {
            let mut nullable = row.clone();
            if explicit_null {
                nullable.insert(null_column.into(), Value::Null);
            } else {
                nullable.remove(null_column);
            }
            assert_eq!(
                scope
                    .evaluate(&Expressions, &table.checks[0].expr, &nullable, &[])
                    .unwrap(),
                Value::Bool(false)
            );
        }
    }
}

#[test]
fn empty_check_rows_keep_the_named_namespace_without_system_fields() {
    let Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE scoped(CHECK(scoped.* IS NOT NULL))")
            .unwrap()
            .remove(0)
    else {
        panic!("CREATE TABLE");
    };
    let scope = CheckRowScope::new(&Catalog, "public.scoped", &[]).unwrap();
    assert!(scope.schema.columns().is_empty());
    assert_eq!(
        scope
            .evaluate(&Expressions, &table.checks[0].expr, &ResultRow::new(), &[])
            .unwrap(),
        Value::Bool(true)
    );
}
