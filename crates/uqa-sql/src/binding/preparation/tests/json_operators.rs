//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn json_operand_analysis_matches_postgresql() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("json_operators.json")).unwrap();
    let mut failures = Vec::new();
    for case in cases.as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let actual = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context());
        if case["error"].is_null() {
            if let Err(error) = actual {
                failures.push(format!("{sql}: {error}"));
            }
        } else {
            match actual {
                Err(error)
                    if error.sqlstate() == case["error"]["sqlstate"].as_str()
                        && error.to_string() == case["error"]["message"].as_str().unwrap()
                        && error.detail() == case["error"]["detail"].as_str()
                        && error.hint() == case["error"]["hint"].as_str() => {}
                actual => failures.push(format!(
                    "{sql}: actual {actual:?}; expected {}",
                    case["error"]
                )),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn fixed_parameter_metadata_preserves_contextual_input_types() {
    let context = crate::binding::fixture::empty_binding_context();
    for (function, expected) in [
        ("json_strip_nulls", ColumnType::Json),
        ("jsonb_strip_nulls", ColumnType::JsonB),
    ] {
        for call in [
            format!("{function}($1, $2)"),
            format!("{function}(strip_in_arrays => $2, target => $1)"),
            format!("pg_catalog.{function}($1, $2)"),
        ] {
            for params in [
                vec![
                    crate::SQLParam::Scalar(uqa_core::Value::Str("{}".into())),
                    crate::SQLParam::Scalar(uqa_core::Value::Str("true".into())),
                ],
                vec![
                    crate::SQLParam::Scalar(uqa_core::Value::Null),
                    crate::SQLParam::Scalar(uqa_core::Value::Bool(false)),
                ],
                vec![
                    crate::SQLParam::typed_scalar(uqa_core::Value::Null, expected.clone()),
                    crate::SQLParam::typed_scalar(uqa_core::Value::Null, ColumnType::Boolean),
                ],
            ] {
                let sql = format!("SELECT {call}, pg_typeof({call}) WHERE false");
                for retained in [false, true] {
                    let mut plan = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
                    if retained {
                        let declared =
                            crate::binding::statements::parameter_input_types(&params).unwrap();
                        let analysis =
                            read_prepared_inputs(&NoRoutines, &mut plan, &declared, &context, None)
                                .unwrap();
                        assert_eq!(
                            analysis.parameter_types,
                            vec![Some(expected.clone()), Some(ColumnType::Boolean)]
                        );
                    }
                    let UnifiedPlan::Query(query) = plan else {
                        panic!("query")
                    };
                    let output = crate::binding::bind_query_plan_schema(
                        &NoRoutines,
                        &query,
                        &params,
                        &context,
                        None,
                    )
                    .unwrap_or_else(|error| panic!("{sql}, retained={retained}: {error}"));
                    assert_eq!(output.column_type(0), Some(&expected));
                    assert_eq!(output.column_type(1), Some(&ColumnType::Regtype));
                }
            }
        }
    }
}

#[test]
fn fixed_parameter_metadata_rejects_declared_text_without_implicit_json_casts() {
    let context = crate::binding::fixture::empty_binding_context();
    for function in ["json_strip_nulls", "jsonb_strip_nulls"] {
        for value in [uqa_core::Value::Str("{}".into()), uqa_core::Value::Null] {
            let params = [crate::SQLParam::typed_scalar(value, ColumnType::Text)];
            let sql = format!("SELECT {function}($1) WHERE false");
            let UnifiedPlan::Query(query) =
                UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
            else {
                panic!("query")
            };
            let error = crate::binding::bind_query_plan_schema(
                &NoRoutines,
                &query,
                &params,
                &context,
                None,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
        }
        let params = [crate::SQLParam::Scalar(uqa_core::Value::Str("{}".into()))];
        let sql = format!("SELECT {function}($1::text) WHERE false");
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let error =
            crate::binding::bind_query_plan_schema(&NoRoutines, &query, &params, &context, None)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(crate::compile("SELECT $1").unwrap().remove(0))
    else {
        panic!("query")
    };
    let output = crate::binding::bind_query_plan_schema(
        &NoRoutines,
        &query,
        &[crate::SQLParam::Scalar(uqa_core::Value::Str("text".into()))],
        &context,
        None,
    )
    .unwrap();
    assert_eq!(output.column_type(0), Some(&ColumnType::Text));
}
