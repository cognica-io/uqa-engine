//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod expanded;

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    kind: String,
    variables: Vec<Variable>,
}

#[derive(serde::Deserialize)]
struct Variable {
    attribute: usize,
    #[serde(rename = "type")]
    type_oid: u32,
    collation: u32,
}

struct Routines;
impl crate::FunctionTypeResolver for Routines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl crate::routines::RoutineResolution for Routines {}

fn inputs() -> (BTreeMap<String, RuleColumnMetadata>, Vec<Row>) {
    let columns = ["c", "p", "plain", "n"]
        .into_iter()
        .enumerate()
        .map(|(position, name)| {
            (
                name.into(),
                RuleColumnMetadata {
                    ty: if name == "n" {
                        ColumnType::Integer
                    } else {
                        ColumnType::Text
                    },
                    collation_oid: [Some(950), Some(951), None, None][position],
                    uses_document_id: false,
                    position,
                },
            )
        })
        .collect();
    let rows = vec![Row(BTreeMap::from([
        ("c".into(), Value::Str("C value".into())),
        ("p".into(), Value::Str("POSIX value".into())),
        ("plain".into(), Value::Null),
        ("n".into(), Value::Int(7)),
    ]))];
    (columns, rows)
}

fn references(kind: &str) -> Vec<Reference> {
    let cases: Vec<Reference> = serde_json::from_str(include_str!("collations.json")).unwrap();
    assert_eq!(
        cases.iter().map(|case| case.variables.len()).sum::<usize>(),
        48
    );
    cases.into_iter().filter(|case| case.kind == kind).collect()
}

fn action(case: &Reference) -> Statement {
    let Statement::CreateRule(mut rule) = crate::compile(&case.sql).unwrap().remove(0) else {
        panic!("rule")
    };
    rule.actions.remove(0)
}

fn check_schema(case: &Reference, select: Box<SelectStmt>) {
    let select = serde_json::from_slice(&serde_json::to_vec(&select).unwrap()).unwrap();
    let crate::plan::UnifiedPlan::Query(query) =
        crate::plan::UnifiedPlan::lower(Statement::Select(select))
    else {
        panic!("query")
    };
    let encoded = serde_json::to_vec(&query).unwrap();
    let restored = serde_json::from_slice(&encoded).unwrap();
    for query in [&query, &restored] {
        let schema = crate::binding::analyze_query_plan_schema(
            &Routines,
            query,
            &[],
            &crate::binding::fixture::empty_binding_context(),
            None,
        )
        .unwrap();
        for variable in &case.variables {
            let position = variable.attribute - 1;
            assert_eq!(
                schema.column_collation(position),
                Some(variable.collation),
                "{}",
                case.sql
            );
            assert_eq!(
                crate::catalog::type_metadata::pg_type_oid(schema.column_type(position).unwrap()),
                i64::from(variable.type_oid),
                "{}",
                case.sql,
            );
        }
    }
}

#[test]
fn rule_values_actions_preserve_postgresql_event_collations() {
    let (columns, rows) = inputs();
    for case in references("values") {
        let action = action(&case);
        let Statement::Insert(bound) =
            bind_insert_values_action(&[0], &rows, &columns, &|resolver| {
                crate::plpgsql::bind_statement(&action, resolver)
            })
            .unwrap()
        else {
            panic!("insert")
        };
        let Statement::Select(mut select) = crate::compile("SELECT 1, 2, 3, 4").unwrap().remove(0)
        else {
            panic!("select")
        };
        for (projection, expression) in select.projections.iter_mut().zip(&bound.rows[0]) {
            projection.expr = expression.clone();
        }
        for (name, expression) in ["c", "p", "plain", "n"].into_iter().zip(&bound.rows[0]) {
            let value =
                crate::expr::eval(expression, &crate::expr::EvalContext::new(None, &[])).unwrap();
            assert_eq!(value, rows[0].0[name]);
        }
        check_schema(&case, select);
    }
}

#[test]
fn rule_set_actions_preserve_postgresql_event_collations_with_empty_inputs() {
    let (columns, rows) = inputs();
    for case in references("select") {
        let action = action(&case);
        for matching in [&[0][..], &[][..]] {
            let bound = bind_set_oriented_action(matching, &rows, &columns, &|resolver| {
                crate::plpgsql::bind_statement(&action, resolver)
            })
            .unwrap();
            let Statement::Insert(insert) = bound.statement else {
                panic!("insert")
            };
            check_schema(&case, insert.select_source.unwrap());
        }
    }
}
