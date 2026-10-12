//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    ast::{ColumnDef, Expr, Statement},
    ColumnType,
};

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    variables: Vec<Variable>,
}
#[derive(serde::Deserialize)]
struct Variable {
    relation: u32,
    attribute: usize,
    #[serde(rename = "type")]
    type_oid: u32,
    collation: u32,
}

fn column_definitions() -> Vec<ColumnDef> {
    ["c", "p", "plain", "n"]
        .into_iter()
        .enumerate()
        .map(|(position, name)| {
            let mut column = ColumnDef::nullable(
                name,
                if name == "n" {
                    ColumnType::Integer
                } else {
                    ColumnType::Text
                },
            );
            column.collation_oid = [Some(950), Some(951), None, None][position];
            column
        })
        .collect()
}

#[test]
fn trigger_when_values_retain_postgresql_types_and_collations() {
    let names = ["c", "p", "plain", "n"];
    let columns = trigger_column_metadata(column_definitions());
    let values = [
        Value::Str("C value".into()),
        Value::Str("POSIX value".into()),
        Value::Null,
        Value::Int(7),
    ];
    let cases: Vec<Reference> = serde_json::from_str(include_str!("collations.json")).unwrap();
    assert_eq!(
        cases.iter().map(|case| case.variables.len()).sum::<usize>(),
        16
    );
    for case in cases {
        let Statement::CreateTrigger(trigger) = uqa_sql::compile(&case.sql).unwrap().remove(0)
        else {
            panic!("trigger")
        };
        for present in [true, false] {
            let row = if present {
                Value::Record(
                    names
                        .iter()
                        .zip(&values)
                        .map(|(name, value)| ((*name).into(), value.clone()))
                        .collect::<Vec<_>>()
                        .into(),
                )
            } else {
                Value::Null
            };
            let mut resolver = TriggerVariableResolver {
                old: &row,
                new: &row,
                columns: &columns,
            };
            let condition = bind_expr(trigger.when.as_ref().unwrap(), &mut resolver).unwrap();
            let plan = uqa_sql::plan::ExpressionPlan::lower(condition);
            assert_eq!(
                crate::eval_scalar(&plan.scalar, &crate::ScalarEvalContext::new(None, &[]))
                    .unwrap(),
                Value::Bool(!present)
            );
            for variable in &case.variables {
                let side = match variable.relation {
                    1 => "old",
                    2 => "new",
                    _ => panic!("event side"),
                };
                let bound = bind_expr(
                    &Expr::qualified_column(side, names[variable.attribute - 1]),
                    &mut resolver,
                )
                .unwrap();
                let bound =
                    serde_json::from_slice::<Expr>(&serde_json::to_vec(&bound).unwrap()).unwrap();
                let Expr::TypedLiteral {
                    ty, collation_oid, ..
                } = &bound
                else {
                    panic!("typed event field")
                };
                let ty = ColumnType::from_sql_name(ty).unwrap();
                assert_eq!(
                    uqa_sql::catalog::type_metadata::pg_type_oid(&ty),
                    i64::from(variable.type_oid)
                );
                assert_eq!(
                    collation_oid.map(i64::from).unwrap_or_else(|| {
                        uqa_sql::catalog::type_metadata::pg_type_collation_oid(&ty)
                    }),
                    i64::from(variable.collation)
                );
                let plan = uqa_sql::plan::ExpressionPlan::lower(bound);
                let value =
                    crate::eval_scalar(&plan.scalar, &crate::ScalarEvalContext::new(None, &[]))
                        .unwrap();
                assert_eq!(
                    value,
                    if present {
                        values[variable.attribute - 1].clone()
                    } else {
                        Value::Null
                    }
                );
            }
        }
    }
}
