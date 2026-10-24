//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use std::collections::HashMap;
use uqa_sql::{ast::ColumnDef, RowSchema};

mod descriptions;

#[derive(Default)]
struct Types {
    resolutions: Cell<usize>,
}
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
        self.resolutions.set(self.resolutions.get() + 1);
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
    let types = Types::default();
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
            descriptions: None,
            types: &types,
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
        assert_eq!(resolver.resolved_datum(2).unwrap().collation_oid, Some(950));
        assert_eq!(
            resolver
                .resolve_qualified("new", "missing")
                .unwrap_err()
                .sqlstate(),
            Some("42703")
        );
    }
}

#[test]
fn prepared_record_parameters_preserve_selected_types_without_text_resolution() {
    let types = Types::default();
    let datums = [PLpgSQLDatum::Rec { name: "row".into() }];
    let bindings = HashMap::from([("row".into(), vec![0])]);
    let reference = uqa_sql::plpgsql::PLpgSQLVariableReference::Qualified {
        qualifier: "row".into(),
        column: "value".into(),
    };
    for (ty, text) in [(ColumnType::SmallInteger, false), (ColumnType::Text, true)] {
        let schema =
            RowSchema::with_column_definitions(None, &[ColumnDef::nullable("value", ty.clone())]);
        let schemas = HashMap::from([(0, schema)]);
        for index in 0..256 {
            let value = if index % 3 == 0 {
                Value::Null
            } else if text {
                Value::Str(index.to_string())
            } else {
                Value::Int(index)
            };
            let values = [Value::Record(vec![("value".into(), value.clone())].into())];
            let mut resolver = DatumResolver {
                descriptions: None,
                types: &types,
                datums: &datums,
                values: &values,
                record_schemas: &schemas,
                bindings: &bindings,
                error: None,
                param_count: 0,
            };
            let parameter = reference.read(&mut resolver).unwrap();
            assert_eq!(parameter.scalar_value(), Some(&value));
            assert_eq!(parameter.declared_scalar_type(), Some(&ty));
        }
    }
    assert_eq!(
        types.resolutions.get(),
        0,
        "a record parameter already owns its selected field type"
    );
}

#[test]
fn prepared_scalar_parameters_do_not_resolve_the_selected_type_twice() {
    let types = Types::default();
    let datums = [PLpgSQLDatum::Var(Box::new(uqa_sql::plpgsql::PLpgSQLVar {
        name: "value".into(),
        type_oid: Some(21),
        type_name: "smallint".into(),
        type_reference: None,
        default: None,
        constant: false,
        not_null: false,
        cursor: None,
        lineno: None,
    }))];
    let bindings = HashMap::from([("value".into(), vec![0])]);
    let schemas = HashMap::new();
    for index in 0..512 {
        let value = if index % 3 == 0 {
            Value::Null
        } else {
            Value::Int(index)
        };
        let values = [value.clone()];
        let mut resolver = DatumResolver {
            descriptions: None,
            types: &types,
            datums: &datums,
            values: &values,
            record_schemas: &schemas,
            bindings: &bindings,
            error: None,
            param_count: 1,
        };
        let parameter = uqa_sql::plpgsql::PLpgSQLVariableReference::Parameter(1)
            .read(&mut resolver)
            .unwrap();
        assert_eq!(parameter.scalar_value(), Some(&value));
        assert_eq!(
            parameter.declared_scalar_type(),
            Some(&ColumnType::SmallInteger)
        );
    }
    assert_eq!(types.resolutions.get(), 512);
}
