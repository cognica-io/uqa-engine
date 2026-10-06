//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{ast::FunctionBinding, ColumnType};

struct Assignment;
impl uqa_sql::FunctionTypeResolver for Assignment {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        unreachable!()
    }
}
impl uqa_sql::routines::RoutineResolution for Assignment {}
impl uqa_sql::expr::EngineHook for Assignment {
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
impl uqa_sql::catalog::domain::DomainCatalog for Assignment {
    fn domain_by_oid(&self, _: u32) -> Option<uqa_sql::catalog::domain::StoredDomain> {
        None
    }
}
impl AssignmentContext for Assignment {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        _: &ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        unreachable!()
    }
}

#[test]
fn selective_rewrite_preserves_other_stored_values_and_full_column_scope() {
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile("CREATE TABLE t(v int,g int GENERATED ALWAYS AS(v+1) STORED,h int GENERATED ALWAYS AS(v+2) STORED,z int GENERATED ALWAYS AS(v+3) VIRTUAL)").unwrap().remove(0) else {panic!("table")};
    for (selected, expected_calls) in [
        (Some(vec!["h".to_string()]), 1),
        (Some(vec![]), 0),
        (None, 2),
    ] {
        let mut document = ResultRow::from([
            ("v".into(), Value::Int(1)),
            ("g".into(), Value::Int(3)),
            ("h".into(), Value::Int(4)),
            ("z".into(), Value::Int(5)),
        ]);
        let mut calls = 0;
        refresh_stored_generated_columns(
            &Assignment,
            &table.columns,
            selected.as_deref(),
            &mut document,
            &mut |_, row, schema| {
                calls += 1;
                assert_eq!(row["v"], Value::Int(1));
                assert_eq!(schema.columns(), &["v", "g", "h", "z"]);
                Ok(Value::Int(42))
            },
        )
        .unwrap();
        assert_eq!(calls, expected_calls);
        assert_eq!(
            document["g"],
            Value::Int(if selected.is_none() { 42 } else { 3 })
        );
        assert_eq!(
            document["h"],
            Value::Int(if expected_calls == 0 { 4 } else { 42 })
        );
        assert!(!document.contains_key("z"));
    }
}
