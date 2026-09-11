//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-backed scalar interception, scoring projections, and highlighting.

use super::{checked_integer_value, BTreeMap, Engine, SQLError, ScalarExpr, Value};
use uqa_execution::query::graph_lifecycle::{
    run_age_alter_graph_with_evaluator, run_age_create_elabel_with_evaluator,
    run_age_create_graph_with_evaluator, run_age_create_vlabel_with_evaluator,
    run_age_drop_graph_with_evaluator, run_age_drop_label_with_evaluator,
    run_age_graph_exists_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator,
};
use uqa_execution::query::scalar_projection::{run_uqa_highlight, score_projection_value};
use uqa_sql::expr::RowLookup;
use uqa_sql::semantics::scalar_projection::validate_score_projection_args;

pub(in crate::sql) fn engine_func_intercept(
    engine: Option<&Engine>,
    name: &str,
    args: &[ScalarExpr],
    row: &dyn RowLookup,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Option<Value>, SQLError> {
    let lower = crate::sql::builtin_function_dispatch_name(name);
    if is_engine_catalog_scalar(&lower) {
        let values = args
            .iter()
            .map(evaluate)
            .collect::<Result<Vec<_>, SQLError>>()?;
        return engine_catalog_scalar_value(
            require_projection_engine(engine, &lower)?,
            &lower,
            &values,
        )
        .transpose();
    }
    match lower.as_str() {
        "uqa_highlight" => Ok(Some(run_uqa_highlight(row, args, evaluate)?)),
        "score_bm25" | "score_bayesian_bm25" => {
            validate_score_projection_args(&lower, args, evaluate)?;
            let score = score_projection_value(&lower, args, row)?;
            Ok(Some(score))
        }
        "deep_learn" => Ok(Some(run_deep_learn_projection(
            require_projection_engine(engine, "deep_learn")?,
            args,
            evaluate,
        )?)),
        "merge_action" => {
            if !args.is_empty() {
                return Err(SQLError::BadArity {
                    name: "merge_action".into(),
                    expected: "0".into(),
                    actual: args.len(),
                });
            }
            let action = row
                .internal_column(crate::sql::merge_action_attribute())
                .cloned()
                .ok_or_else(|| {
                    SQLError::Unsupported("merge_action() is only valid in MERGE RETURNING".into())
                })?;
            Ok(Some(action))
        }
        // UQA-native helpers keep their lenient semantics.
        "graph_create" => {
            let eng = require_projection_engine(engine, "graph_create")?;
            Ok(Some(Value::Bool(run_graph_create_with_evaluator(
                eng, args, evaluate,
            )?)))
        }
        "graph_drop" => {
            let eng = require_projection_engine(engine, "graph_drop")?;
            Ok(Some(Value::Bool(run_graph_drop_with_evaluator(
                eng, args, evaluate,
            )?)))
        }
        // Apache AGE-compatible functions: strict name validation and
        // a void (SQL NULL) return value.
        "create_graph" => Ok(Some(run_age_create_graph_with_evaluator(
            require_projection_engine(engine, "create_graph")?,
            args,
            evaluate,
        )?)),
        "drop_graph" => Ok(Some(run_age_drop_graph_with_evaluator(
            require_projection_engine(engine, "drop_graph")?,
            args,
            evaluate,
        )?)),
        "graph_exists" => Ok(Some(run_age_graph_exists_with_evaluator(
            require_projection_engine(engine, "graph_exists")?,
            args,
            evaluate,
        )?)),
        "create_vlabel" => Ok(Some(run_age_create_vlabel_with_evaluator(
            require_projection_engine(engine, "create_vlabel")?,
            args,
            evaluate,
        )?)),
        "create_elabel" => Ok(Some(run_age_create_elabel_with_evaluator(
            require_projection_engine(engine, "create_elabel")?,
            args,
            evaluate,
        )?)),
        "drop_label" => Ok(Some(run_age_drop_label_with_evaluator(
            require_projection_engine(engine, "drop_label")?,
            args,
            evaluate,
        )?)),
        "alter_graph" => Ok(Some(run_age_alter_graph_with_evaluator(
            require_projection_engine(engine, "alter_graph")?,
            args,
            evaluate,
        )?)),
        _ => Ok(None),
    }
}

fn is_engine_catalog_scalar(name: &str) -> bool {
    matches!(
        name,
        "pg_get_expr"
            | "pg_get_partkeydef"
            | "pg_backend_pid"
            | "pg_notify"
            | "pg_notification_queue_usage"
            | "pg_get_serial_sequence"
            | "pg_get_sequence_data"
            | "pg_sequence_last_value"
            | "pg_sequence_parameters"
            | "pg_get_triggerdef"
            | "pg_get_ruledef"
            | "pg_get_viewdef"
            | "pg_get_indexdef"
            | "format_type"
            | "pg_has_role"
            | "has_database_privilege"
            | "has_schema_privilege"
            | "has_sequence_privilege"
    )
}

pub(in crate::sql) fn engine_catalog_scalar_value(
    engine: &Engine,
    name: &str,
    arguments: &[Value],
) -> Option<Result<Value, SQLError>> {
    let lower = crate::sql::builtin_function_dispatch_name(name);
    Some(match lower.as_str() {
        "pg_backend_pid" => match arguments {
            [] => Ok(Value::Int(i64::from(engine.backend_process_id()))),
            _ => Err(SQLError::BadArity {
                name: lower,
                expected: "0".into(),
                actual: arguments.len(),
            }),
        },
        "pg_notify" => match arguments {
            [channel, payload] => (|| {
                let channel = notification_text_argument(channel, "channel")?;
                let payload = notification_text_argument(payload, "payload")?;
                engine.notify(channel, payload)?;
                Ok(Value::Void)
            })(),
            _ => Err(SQLError::BadArity {
                name: lower,
                expected: "2".into(),
                actual: arguments.len(),
            }),
        },
        "pg_notification_queue_usage" => match arguments {
            [] => engine.notification_queue_usage().map(Value::Float),
            _ => Err(SQLError::BadArity {
                name: lower,
                expected: "0".into(),
                actual: arguments.len(),
            }),
        },
        "pg_get_expr" => crate::sql::catalog::pg_get_expr_value(engine, arguments),
        "pg_get_partkeydef" => crate::sql::catalog::pg_get_partkeydef_value(engine, arguments),
        "pg_get_triggerdef" => crate::sql::catalog::pg_get_triggerdef_value(engine, arguments),
        "pg_get_ruledef" => crate::sql::catalog::pg_get_ruledef_value(engine, arguments),
        "pg_get_viewdef" => crate::sql::catalog::pg_get_viewdef_value(engine, arguments),
        "format_type" => crate::sql::catalog::format_type_value(engine, arguments),
        "pg_get_indexdef" => crate::sql::catalog::pg_get_indexdef_value(engine, arguments),
        "pg_get_serial_sequence" => engine.pg_get_serial_sequence_value(arguments),
        "pg_get_sequence_data" => engine.pg_get_sequence_data_value(arguments),
        "pg_sequence_last_value" => engine.pg_sequence_last_value_value(arguments),
        "pg_sequence_parameters" => engine.pg_sequence_parameters_value(arguments),
        "pg_has_role" => engine.pg_has_role_value(arguments),
        "has_table_privilege" => engine.has_table_privilege_value(arguments),
        "has_column_privilege" => engine.has_column_privilege_value(arguments),
        "has_database_privilege" => engine.has_database_privilege_value(arguments),
        "has_schema_privilege" => engine.has_schema_privilege_value(arguments),
        "has_sequence_privilege" => engine.has_sequence_privilege_value(arguments),
        _ => return None,
    })
}

fn notification_text_argument<'a>(value: &'a Value, label: &str) -> Result<&'a str, SQLError> {
    match value {
        Value::Null => Ok(""),
        Value::Str(value) => Ok(value),
        Value::FixedChar(value) => Ok(value.trim_end_matches(' ')),
        other => Err(SQLError::TypeMismatch(format!(
            "pg_notify {label} must be text, got {other:?}"
        ))),
    }
}

pub(in crate::sql) fn require_projection_engine<'a>(
    engine: Option<&'a Engine>,
    function: &str,
) -> Result<&'a Engine, SQLError> {
    engine.ok_or_else(|| {
        SQLError::Unsupported(format!("{function} requires an engine-backed projection"))
    })
}

pub(in crate::sql) fn run_deep_learn_projection(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "deep_learn".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let model_name = match evaluate(&args[0])? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.model must be a string, got {other:?}"
            )));
        }
    };
    let training_source = match evaluate(&args[1])? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.training_set must be a table name or JSON string, got {other:?}"
            )));
        }
    };
    let trimmed = training_source.trim();
    let output = if trimmed.starts_with('{') {
        engine.deep_learn_json(&model_name, trimmed, &uqa_ml::LearnOptions::default())?
    } else {
        engine.deep_learn_table(
            &model_name,
            &training_source,
            &uqa_ml::LearnOptions::default(),
        )?
    };
    let mut report = BTreeMap::new();
    report.insert("model".into(), Value::Str(model_name));
    report.insert(
        "examples".into(),
        checked_integer_value(output.report.examples, "training example count")?,
    );
    report.insert(
        "feature_dimensions".into(),
        checked_integer_value(output.report.feature_dimensions, "feature dimension count")?,
    );
    report.insert(
        "class_count".into(),
        checked_integer_value(output.report.class_count, "class count")?,
    );
    Ok(Value::Map(report))
}
