//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-backed scalar interception, scoring projections, and highlighting.

use super::{
    checked_integer_value, expect_column_name, run_age_alter_graph_with_evaluator,
    run_age_create_elabel_with_evaluator, run_age_create_graph_with_evaluator,
    run_age_create_vlabel_with_evaluator, run_age_drop_graph_with_evaluator,
    run_age_drop_label_with_evaluator, run_age_graph_exists_with_evaluator,
    run_graph_create_with_evaluator, run_graph_drop_with_evaluator, BTreeMap, Engine, SQLError,
    ScalarExpr, Value,
};
use uqa_sql::expr::RowLookup;

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
            | "pg_get_serial_sequence"
            | "pg_get_sequence_data"
            | "pg_sequence_last_value"
            | "pg_sequence_parameters"
            | "pg_get_triggerdef"
            | "pg_get_ruledef"
            | "pg_has_role"
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
        "pg_get_expr" => crate::sql::catalog::pg_get_expr_value(engine, arguments),
        "pg_get_partkeydef" => crate::sql::catalog::pg_get_partkeydef_value(engine, arguments),
        "pg_get_triggerdef" => crate::sql::catalog::pg_get_triggerdef_value(engine, arguments),
        "pg_get_ruledef" => crate::sql::catalog::pg_get_ruledef_value(engine, arguments),
        "pg_get_serial_sequence" => engine.pg_get_serial_sequence_value(arguments),
        "pg_get_sequence_data" => engine.pg_get_sequence_data_value(arguments),
        "pg_sequence_last_value" => engine.pg_sequence_last_value_value(arguments),
        "pg_sequence_parameters" => engine.pg_sequence_parameters_value(arguments),
        "pg_has_role" => engine.pg_has_role_value(arguments),
        "has_sequence_privilege" => engine.has_sequence_privilege_value(arguments),
        _ => return None,
    })
}

pub(in crate::sql) fn score_projection_value(
    function: &str,
    args: &[ScalarExpr],
    row: &dyn RowLookup,
) -> Result<Value, SQLError> {
    let qualifier = (args.len() == 2)
        .then(|| match &args[0] {
            ScalarExpr::QualifiedColumn { qualifier, .. } => Some(qualifier.as_str()),
            _ => None,
        })
        .flatten();
    if row.score_source_is_ambiguous(qualifier) {
        return Err(SQLError::Unsupported(format!(
            "{function}() has multiple score-bearing retrieval rows; qualify its field argument"
        )));
    }
    if let Some(Value::Float(score)) = row.score_source(qualifier) {
        return Ok(Value::Float(*score));
    }
    Err(score_projection_context_error(function))
}

pub(in crate::sql) fn score_projection_context_error(function: &str) -> SQLError {
    SQLError::Unsupported(format!(
        "{function}() requires a score-bearing retrieval row"
    ))
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

pub(in crate::sql) fn validate_score_projection_args(
    name: &str,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<(), SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "1..=2".into(),
            actual: args.len(),
        });
    }
    let query_idx = args.len() - 1;
    if args.len() == 2 {
        let _ = expect_column_name(&args[0], &format!("{name}.field"))?;
    }
    match evaluate(&args[query_idx])? {
        Value::Str(_) => Ok(()),
        other => Err(SQLError::TypeMismatch(format!(
            "{name}.query must be a string, got {other:?}"
        ))),
    }
}

/// Evaluate a `uqa_highlight(field, query[, start_tag, end_tag,
/// max_fragments, fragment_size])` projection. `field` can be either a
/// bare column reference (looked up on the row) or a literal string;
/// the rest of the args are scalar literals after evaluation.
#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(in crate::sql) fn run_uqa_highlight(
    row: &dyn RowLookup,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if args.len() < 2 || args.len() > 6 {
        return Err(SQLError::BadArity {
            name: "uqa_highlight".into(),
            expected: "2..=6".into(),
            actual: args.len(),
        });
    }
    let text = match &args[0] {
        ScalarExpr::Column(c) => match row.column(c) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Null) => return Ok(Value::Null),
            Some(other) => format!("{other:?}"),
            None => return Ok(Value::Null),
        },
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            match row.qualified_column(qualifier, column) {
                Some(Value::Str(s)) => s.clone(),
                Some(Value::Null) => return Ok(Value::Null),
                Some(other) => format!("{other:?}"),
                None => return Ok(Value::Null),
            }
        }
        other => match evaluate(other)? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            v => format!("{v:?}"),
        },
    };
    let query_str = match evaluate(&args[1])? {
        Value::Str(s) => s,
        Value::Null => return Ok(Value::Str(text)),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "uqa_highlight query must be string, got {other:?}"
            )));
        }
    };
    let start_tag = match args.get(2) {
        Some(e) => match evaluate(e)? {
            Value::Str(s) => s,
            Value::Null => "<b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight start_tag must be string, got {other:?}"
                )));
            }
        },
        None => "<b>".into(),
    };
    let end_tag = match args.get(3) {
        Some(e) => match evaluate(e)? {
            Value::Str(s) => s,
            Value::Null => "</b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight end_tag must be string, got {other:?}"
                )));
            }
        },
        None => "</b>".into(),
    };
    let max_fragments = match args.get(4) {
        Some(e) => match evaluate(e)? {
            Value::Int(n) if n >= 0 => usize::try_from(n).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments {n} exceeds the platform usize range"
                ))
            })?,
            Value::Null => 0,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments must be non-negative integer, got {other:?}"
                )));
            }
        },
        None => 0,
    };
    let fragment_size = match args.get(5) {
        Some(e) => match evaluate(e)? {
            Value::Int(n) if n > 0 => usize::try_from(n).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size {n} exceeds the platform usize range"
                ))
            })?,
            Value::Null => 150,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size must be positive integer, got {other:?}"
                )));
            }
        },
        None => 150,
    };
    let opts = uqa_analysis::HighlightOptions {
        start_tag,
        end_tag,
        max_fragments,
        fragment_size,
    };
    // Pull every whitespace-separated token from the query string as a
    // candidate match term. A simple split matches the documented highlighting
    // surface and its regression fixtures.
    let terms: Vec<String> = query_str
        .split_whitespace()
        .filter(|t| !matches!(t.to_ascii_lowercase().as_str(), "and" | "or" | "not"))
        .map(std::string::ToString::to_string)
        .collect();
    let analyzer = uqa_analysis::standard_analyzer("english");
    let out = uqa_analysis::highlight(&text, &terms, Some(&analyzer), &opts)
        .map_err(|error| SQLError::Internal(format!("highlight analysis failed: {error}")))?;
    Ok(Value::Str(out))
}
