//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` prepared-result metadata observed after input type selection.

use super::{Catalog, ColumnType, Diagnostic, RetainedCatalog};
use crate::{binding, plan::UnifiedPlan};

#[derive(serde::Deserialize)]
struct Case {
    sql: String,
    declared: Vec<Option<String>>,
    parameter_types: Option<Vec<String>>,
    result_oids: Option<Vec<i64>>,
    error: Option<Diagnostic>,
    clock: i64,
}

#[test]
fn prepared_collations_keep_postgresql_result_types_after_input_coercion() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../collation_prepared_results.json")).unwrap();
    let mut failures = Vec::new();
    for case in cases {
        let _clock = crate::expr::TransactionClockScope::enter(case.clock);
        let context = binding::fixture::empty_binding_context();
        let mut plan = UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0));
        let declared = case
            .declared
            .iter()
            .map(|name| {
                name.as_deref()
                    .map(|name| ColumnType::from_sql_name(name).unwrap())
            })
            .collect::<Vec<_>>();
        let inferred =
            binding::infer_prepared_parameter_types(&Catalog, &plan, &declared, &context);
        let prepared =
            binding::read_prepared_inputs(&Catalog, &mut plan, &declared, &context, None);
        if let Some(error) = &case.error {
            error.assert(&inferred.unwrap_err(), &case.sql);
            error.assert(&prepared.unwrap_err(), &case.sql);
            continue;
        }
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                failures.push(format!("{}: preparation: {error}", case.sql));
                continue;
            }
        };
        let expected = case
            .parameter_types
            .as_ref()
            .unwrap()
            .iter()
            .map(|name| Some(ColumnType::from_sql_name(name).unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(prepared.parameter_types, expected, "{}", case.sql);
        assert_eq!(inferred.unwrap(), expected, "{}", case.sql);
        let parameters = crate::prepared::analysis_parameters(&prepared.parameter_types);
        match restored_result_oids(&case, &plan, &parameters) {
            Ok(actual) => {
                if Some(&actual) != case.result_oids.as_ref() {
                    failures.push(format!(
                        "{}: result types {actual:?}, expected {:?}",
                        case.sql, case.result_oids
                    ));
                }
            }
            Err(error) => failures.push(format!("{}: retained analysis: {error}", case.sql)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn restored_result_oids(
    case: &Case,
    plan: &UnifiedPlan,
    parameters: &[crate::SQLParam],
) -> Result<Vec<i64>, crate::SQLError> {
    plan.visit_scalar_expressions(&mut |expression| {
        expression
            .try_visit(&mut |node| {
                if let Some(clause) = crate::catalog::collations::expression::scalar_clause(node)? {
                    assert!(
                        clause.oid.is_some(),
                        "{}: collation identity was not retained",
                        case.sql
                    );
                }
                Ok::<_, crate::SQLError>(true)
            })
            .unwrap();
    });
    let encoded = serde_json::to_vec(plan).unwrap();
    let restored = serde_json::from_slice::<UnifiedPlan>(&encoded).unwrap();
    let UnifiedPlan::Query(query) = restored else {
        panic!("query reference");
    };
    let context = binding::fixture::empty_binding_context();
    let _later = crate::expr::TransactionClockScope::enter(case.clock + 86_400_000_000);
    binding::analyze_query_plan_schema_with_catalog(
        &RetainedCatalog,
        &query,
        parameters,
        context.catalog,
        context.resolution,
    )
    .map(|schema| {
        schema
            .column_types()
            .iter()
            .map(|ty| {
                ty.as_ref()
                    .map(crate::catalog::type_metadata::pg_type_oid)
                    .unwrap_or(705)
            })
            .collect()
    })
}

#[test]
fn collated_membership_retains_each_definition_time_input_after_serialization() {
    use crate::ScalarExpr;
    #[derive(serde::Deserialize)]
    struct Input {
        oid: i64,
        text: String,
    }
    #[derive(serde::Deserialize)]
    struct MembershipCase {
        sql: String,
        clock: i64,
        inputs: Vec<Input>,
    }
    let cases: Vec<MembershipCase> =
        serde_json::from_str(include_str!("../collation_prepared_inputs.json")).unwrap();
    for case in cases {
        let _clock = crate::expr::TransactionClockScope::enter(case.clock);
        let context = binding::fixture::empty_binding_context();
        let mut plan = UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0));
        binding::read_prepared_inputs(&Catalog, &mut plan, &[], &context, None).unwrap();
        let encoded = serde_json::to_vec(&plan).unwrap();
        let restored: UnifiedPlan = serde_json::from_slice(&encoded).unwrap();
        let UnifiedPlan::Query(query) = &restored else {
            panic!("query")
        };
        let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block")
        };
        let ScalarExpr::Or(comparisons) = &block.projections[0].expr else {
            panic!("per-field comparisons")
        };
        assert_eq!(comparisons.len(), case.inputs.len());
        let _later = crate::expr::TransactionClockScope::enter(case.clock + 86_400_000_000);
        for (comparison, expected) in comparisons.iter().zip(&case.inputs) {
            let ScalarExpr::Binary { lhs, .. } = comparison else {
                panic!("comparison")
            };
            let mut constant = lhs.as_ref();
            while let Some(clause) =
                crate::catalog::collations::expression::scalar_clause(constant).unwrap()
            {
                assert!(clause.oid.is_some());
                constant = clause.argument;
            }
            let ScalarExpr::TypedLiteral {
                value,
                bound_type: Some(ty),
                ..
            } = constant
            else {
                panic!("retained input: {constant:?}")
            };
            assert_eq!(
                crate::catalog::type_metadata::pg_type_oid(ty),
                expected.oid,
                "{}",
                case.sql
            );
            let text =
                crate::expr::cast_value_from(value, "text", Some(&ty.catalog_name())).unwrap();
            assert_eq!(
                text,
                uqa_core::Value::Str(expected.text.clone()),
                "{}",
                case.sql
            );
        }
    }
}
