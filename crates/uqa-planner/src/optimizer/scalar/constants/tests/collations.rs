//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(serde::Deserialize)]
struct Reference {
    field: String,
    null: bool,
    collation: u32,
    value: Option<String>,
}

#[test]
fn selected_constant_fields_retain_postgresql_collations() {
    let cases: Vec<Reference> =
        serde_json::from_str(include_str!("field_collations.json")).unwrap();
    assert_eq!(cases.len(), 4);
    for case in cases {
        let ty = ColumnType::Composite(uqa_sql::ast::CompositeTypeReference {
            schema: "public".into(),
            name: "pair".into(),
            oid: 20_001,
            relation_oid: 20_002,
            array_oid: 20_003,
        });
        let value = if case.null {
            Value::Null
        } else {
            Value::Record(
                vec![
                    ("c".into(), Value::Str("x".into())),
                    ("p".into(), Value::Str("y".into())),
                ]
                .into(),
            )
        };
        let mut expression = bound_call(
            "field selection",
            vec![
                ScalarExpr::TypedLiteral {
                    collation_oid: None,
                    composite_source: None,
                    value,
                    ty: ty.catalog_name(),
                    bound_type: Some(ty),
                    parameter_index: None,
                },
                ScalarExpr::Literal(Value::Str(case.field.clone())),
            ],
        );
        if let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = &mut expression
        {
            binding.dispatch = Some(uqa_sql::ast::FunctionDispatch::FieldSelect);
            binding.composite_field = Some(Box::new(uqa_sql::ast::CompositeFieldBinding {
                collation_oid: Some(case.collation),
                type_oid: 20_001,
                number: if case.field == "c" { 1 } else { 2 },
                result_type: ColumnType::Text,
                dropped: false,
                changed_type: None,
            }));
        }
        let folded =
            fold_literal_expression(expression, |_| panic!("constant field already read")).unwrap();
        assert_eq!(
            literal_value(&folded),
            Some(&case.value.map_or(Value::Null, Value::Str))
        );
        let stored = serde_json::to_value(&folded).unwrap();
        assert_eq!(
            stored["TypedLiteral"]["collation_oid"],
            serde_json::json!(case.collation)
        );
    }
}

mod fixture;

#[derive(serde::Deserialize)]
struct ComputedReference {
    query: String,
    null: bool,
    collation: u32,
    value: Option<String>,
}

#[test]
fn folding_constant_expressions_preserves_selected_output_collation() {
    use uqa_sql::binding::statements::{analyze_plan_result, AnalyzedResult};
    use uqa_sql::plan::{RelationalPlan, UnifiedPlan};
    let cases: Vec<ComputedReference> =
        serde_json::from_str(include_str!("collations/computed.json")).unwrap();
    assert_eq!(cases.len(), 40);
    for case in cases {
        let mut plan = UnifiedPlan::lower(uqa_sql::compile(&case.query).unwrap().remove(0));
        plan.rewrite_scalar_expressions(&mut |node| {
            if let ScalarExpr::Column(name) = node {
                let (text, oid) = match name.as_str() {
                    "c" => ("x", 950),
                    "p" => ("y", 951),
                    _ => panic!("fixture column"),
                };
                *node = ScalarExpr::TypedLiteral {
                    value: if case.null {
                        Value::Null
                    } else {
                        Value::Str(text.into())
                    },
                    ty: ColumnType::Text.catalog_name(),
                    bound_type: Some(ColumnType::Text),
                    parameter_index: None,
                    composite_source: None,
                    collation_oid: Some(oid),
                };
            }
        });
        let catalog = fixture::NoRoutines;
        let before = analyze_plan_result(&catalog, &catalog, &mut plan, &[], &catalog)
            .unwrap_or_else(|error| panic!("{}: {error}", case.query));
        let AnalyzedResult::Schema(before) = before else {
            panic!("query schema")
        };
        assert_eq!(
            before.column_collation(0),
            Some(case.collation),
            "{}",
            case.query
        );
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        let projection = &mut block.projections[0];
        let original_label = uqa_sql::semantics::projection_label_at(projection);
        let budget = uqa_core::memory::MemoryBudget::new(8192);
        let token = uqa_core::CancellationToken::new();
        let control = uqa_core::memory::ProductionControl::new(&budget, &token, &token);
        let copied = projection.expr.clone_with_control(&control).unwrap();
        assert_eq!(*copied, projection.expr);
        drop(copied);
        assert_eq!(budget.used(), 0);
        token.cancel();
        assert!(projection.expr.clone_with_control(&control).is_err());
        assert_eq!(budget.used(), 0);
        let mut config = crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
        config.coerced_conditionals = true;
        crate::optimizer::optimize_scalar_expression(&mut projection.expr, &config).unwrap();
        let value = uqa_execution::scalar::eval_constant_scalar(&projection.expr)
            .unwrap_or_else(|error| panic!("{}: {error}", case.query));
        let text = uqa_sql::expr::cast_value(&value, "text").unwrap();
        assert_eq!(
            text,
            case.value.map_or(Value::Null, Value::Str),
            "{}",
            case.query
        );
        projection.alias = Some(original_label);
        let restored: UnifiedPlan =
            serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
        for mut plan in [plan, restored] {
            let after = analyze_plan_result(&catalog, &catalog, &mut plan, &[], &catalog).unwrap();
            assert_eq!(
                after,
                AnalyzedResult::Schema(before.clone()),
                "{}",
                case.query
            );
            let once = serde_json::to_value(&plan).unwrap();
            analyze_plan_result(&catalog, &catalog, &mut plan, &[], &catalog).unwrap();
            assert_eq!(
                serde_json::to_value(&plan).unwrap(),
                once,
                "reanalysis must not accumulate wrappers: {}",
                case.query
            );
        }
    }
}

#[test]
fn simplified_case_retains_selected_branch_modifiers_and_collation() {
    use uqa_sql::binding::statements::{analyze_plan_result, AnalyzedResult};
    use uqa_sql::plan::{RelationalPlan, UnifiedPlan};
    for (condition, selected, expected) in [(true, "abc", 7), (false, "x", 13)] {
        let sql =
            format!("SELECT CASE WHEN {condition} THEN 'abc'::varchar(3) ELSE 'x'::varchar(9) END");
        let catalog = fixture::NoRoutines;
        let mut plan = UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0));
        analyze_plan_result(&catalog, &catalog, &mut plan, &[], &catalog).unwrap();
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        let mut config = crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
        config.coerced_conditionals = true;
        crate::optimizer::optimize_scalar_expression(&mut block.projections[0].expr, &config)
            .unwrap();
        assert_eq!(
            uqa_execution::scalar::eval_constant_scalar(&block.projections[0].expr).unwrap(),
            Value::Str(selected.into())
        );
        let once = serde_json::to_value(&plan).unwrap();
        plan = serde_json::from_value(once).unwrap();
        let AnalyzedResult::Schema(schema) =
            analyze_plan_result(&catalog, &catalog, &mut plan, &[], &catalog).unwrap()
        else {
            panic!("schema")
        };
        assert_eq!(
            uqa_sql::catalog::type_metadata::pg_type_modifier(schema.column_type(0).unwrap()),
            expected
        );
        assert_eq!(schema.column_collation(0), Some(100));
    }
}

#[test]
fn nested_collated_calls_fold_on_an_ordinary_thread_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            use uqa_sql::binding::statements::{analyze_plan_result, AnalyzedResult};
            use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

            let concatenation = vec!["chr(65)"; 32].join(" || ");
            let sql = format!("SELECT lower(({concatenation}) COLLATE \"C\")");
            let mut plan = UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0));
            let catalog = fixture::NoRoutines;
            let before = analyze_plan_result(&catalog, &catalog, &mut plan, &[], &catalog).unwrap();
            let AnalyzedResult::Schema(schema) = &before else {
                panic!("query schema");
            };
            assert_eq!(schema.column_collation(0), Some(950));
            let UnifiedPlan::Query(query) = &mut plan else {
                panic!("query plan");
            };
            let RelationalPlan::QueryBlock(block) = &mut query.root else {
                panic!("query block");
            };
            let projection = &mut block.projections[0];
            let label = uqa_sql::semantics::projection_label_at(projection);
            let expression = &mut projection.expr;
            let config = crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar);
            crate::optimizer::optimize_scalar_expression(expression, &config).unwrap();
            assert_eq!(
                uqa_execution::scalar::eval_constant_scalar(expression).unwrap(),
                Value::Str("a".repeat(32))
            );
            projection.alias = Some(label);
            assert_eq!(
                analyze_plan_result(&catalog, &catalog, &mut plan, &[], &catalog).unwrap(),
                before
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
