//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct RecordVariables(bool);

impl VariableResolver for RecordVariables {
    fn resolve_name(&mut self, _: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier != "r" {
            return Ok(None);
        }
        let oid = match column {
            "c" => 950,
            "p" => 951,
            "plain" => 100,
            _ => return Ok(None),
        };
        Ok(Some(ResolvedVariable {
            value: if self.0 {
                Value::Null
            } else {
                Value::Str("value".into())
            },
            declared_type: Some("text".into()),
            collation_oid: Some(oid),
        }))
    }

    fn resolve_param(&mut self, _: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

struct NoRoutines;
impl crate::FunctionTypeResolver for NoRoutines {
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
impl crate::routines::RoutineResolution for NoRoutines {}

#[derive(serde::Deserialize)]
struct Reference {
    query: String,
    null: bool,
    collation: u32,
}

#[test]
fn procedural_parameters_preserve_postgresql_implicit_collations() {
    let cases: Vec<Reference> = serde_json::from_str(include_str!("collations.json")).unwrap();
    assert_eq!(cases.len(), 18);
    let context = crate::binding::fixture::empty_binding_context();
    for case in cases {
        let statement = crate::compile(&case.query).unwrap().remove(0);
        let bound = parameterize_statement_variables(
            &statement,
            &mut RecordVariables(case.null),
            VariableConflict::Error,
            &mut |_, _, sites| Ok(vec![VariableSiteResolution::Variable; sites.len()]),
            &|_: &str| false,
        )
        .unwrap();
        let declared = bound
            .parameters
            .iter()
            .map(|value| value.declared_scalar_type().cloned())
            .collect::<Vec<_>>();
        let mut plan = bound.plan;
        crate::binding::read_prepared_inputs(&NoRoutines, &mut plan, &declared, &context, None)
            .unwrap();
        let restored = serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
        for plan in [&plan, &restored] {
            assert_eq!(
                crate::prepared::analyze_prepared_plan(&NoRoutines, plan, &declared, &context)
                    .unwrap()
                    .unwrap()
                    .column_collation(0),
                Some(case.collation),
                "{} prepared",
                case.query
            );
            let crate::plan::UnifiedPlan::Query(query) = plan else {
                panic!("query")
            };
            for result in [
                crate::binding::analyze_query_plan_schema(
                    &NoRoutines,
                    query,
                    &bound.parameters,
                    &context,
                    None,
                ),
                crate::binding::bind_query_plan_schema(
                    &NoRoutines,
                    query,
                    &bound.parameters,
                    &context,
                    None,
                ),
            ] {
                assert_eq!(
                    result.unwrap().column_collation(0),
                    Some(case.collation),
                    "{} NULL={}",
                    case.query,
                    case.null
                );
            }
        }
        for reference in &bound.references {
            assert_eq!(
                reference
                    .read(&mut RecordVariables(!case.null))
                    .unwrap()
                    .scalar_value(),
                Some(&if case.null {
                    Value::Str("value".into())
                } else {
                    Value::Null
                })
            );
        }
    }
}
