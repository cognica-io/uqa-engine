//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::HashMap;
use uqa_sql::{ast::ColumnDef, RowSchema};

struct Types;
impl uqa_sql::catalog::domain::DomainCatalog for Types {
    fn domain_by_oid(&self, _: u32) -> Option<uqa_sql::catalog::domain::StoredDomain> {
        None
    }
}
impl uqa_sql::expr::EngineHook for Types {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("unexpected sequence")
    }
}
impl uqa_sql::assignment::AssignmentContext for Types {
    fn evaluate_domain_check(
        &self,
        _: &uqa_sql::ast::Expr,
        _: &uqa_sql::ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        panic!("unexpected domain check")
    }
}
impl uqa_sql::assignment::routines::RoutineValueContext for Types {
    fn catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }
}

fn record_schema() -> RowSchema {
    let columns = ["c", "p", "plain", "n"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut column = ColumnDef::nullable(
                *name,
                if i == 3 {
                    ColumnType::Integer
                } else {
                    ColumnType::Text
                },
            );
            column.collation_oid = [Some(950), Some(951), None, None][i];
            column
        })
        .collect::<Vec<_>>();
    RowSchema::with_column_definitions(None, &columns)
}

#[test]
fn routine_record_fields_preserve_postgresql_declared_collations() {
    let names = ["c", "p", "plain", "n"];
    let schema = record_schema();
    let schemas = HashMap::from([(0, schema.clone()), (1, schema)]);
    let datums = [
        PLpgSQLDatum::Rec { name: "old".into() },
        PLpgSQLDatum::Rec { name: "new".into() },
        PLpgSQLDatum::RecField {
            parent: 1,
            field: "c".into(),
        },
    ];
    let bindings = HashMap::from([("old".into(), vec![0]), ("new".into(), vec![1])]);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../mutation/triggers/tests/collations.json"
    ))
    .unwrap();
    for null in [false, true] {
        let fields = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                (
                    (*name).into(),
                    if null {
                        Value::Null
                    } else if i == 3 {
                        Value::Int(7)
                    } else {
                        Value::Str((*name).into())
                    },
                )
            })
            .collect::<Vec<_>>();
        let record = Value::Record(fields.into());
        let values = [record.clone(), record, Value::Null];
        let mut resolver = DatumResolver {
            types: &Types,
            datums: &datums,
            values: &values,
            record_schemas: &schemas,
            bindings: &bindings,
            error: None,
            param_count: 0,
        };
        let mut observed = 0;
        for case in reference.as_array().unwrap() {
            for variable in case["variables"].as_array().unwrap() {
                let relation = if variable["relation"] == 1 {
                    "old"
                } else {
                    "new"
                };
                let position =
                    usize::try_from(variable["attribute"].as_u64().unwrap()).unwrap() - 1;
                let resolved = resolver
                    .resolve_qualified(relation, names[position])
                    .unwrap()
                    .unwrap();
                let ty =
                    ColumnType::from_sql_name(resolved.declared_type.as_deref().unwrap()).unwrap();
                assert_eq!(
                    uqa_sql::catalog::type_metadata::pg_type_oid(&ty),
                    variable["type"].as_i64().unwrap()
                );
                assert_eq!(
                    resolved.collation_oid.map(u64::from),
                    variable["collation"].as_u64()
                );
                assert_eq!(
                    resolved.value,
                    if null {
                        Value::Null
                    } else if position == 3 {
                        Value::Int(7)
                    } else {
                        Value::Str(names[position].into())
                    }
                );
                observed += 1;
            }
        }
        assert_eq!(observed, 16);
        assert_eq!(resolver.resolved_datum(2).collation_oid, Some(950));
        assert_eq!(
            resolver
                .resolve_qualified("new", "missing")
                .unwrap_err()
                .sqlstate(),
            Some("42703")
        );
    }
}
