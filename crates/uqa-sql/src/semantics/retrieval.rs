//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval argument binding and weight semantics, independent of physical search.

use crate::{SQLError, ScalarExpr};
use uqa_core::Value;

pub type ArgumentEvaluator<'a> = dyn FnMut(&ScalarExpr) -> Result<Value, SQLError> + 'a;

pub fn expect_string(
    expr: &ScalarExpr,
    name: &str,
    evaluate: &mut ArgumentEvaluator<'_>,
) -> Result<String, SQLError> {
    expect_evaluated_string(evaluate(expr)?, name)
}

pub fn expect_evaluated_string(value: Value, name: &str) -> Result<String, SQLError> {
    match value {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a string, got {other:?}"
        ))),
    }
}

use super::expect_column_name;

pub fn expect_field_name_or_string(
    expr: &ScalarExpr,
    label: &str,
    evaluate: &mut ArgumentEvaluator<'_>,
) -> Result<String, SQLError> {
    match expr {
        ScalarExpr::Column(name) => Ok(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Ok(column.clone()),
        _ => expect_string(expr, label, evaluate),
    }
}

pub fn expect_usize(
    expr: &ScalarExpr,
    label: &str,
    evaluate: &mut ArgumentEvaluator<'_>,
) -> Result<usize, SQLError> {
    let v = evaluate(expr)?;
    match v {
        Value::Int(n) if n >= 0 => usize::try_from(n).map_err(|_| {
            SQLError::TypeMismatch(format!("{label} exceeds the platform usize range"))
        }),
        Value::Int(_) => Err(SQLError::TypeMismatch(format!("{label} must be >= 0"))),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be an integer, got {other:?}"
        ))),
    }
}

pub type MultiFieldMatchArgs = (Vec<String>, Vec<String>, Vec<f64>);

use super::MultiFieldMatchShape;

use super::multi_field_match_shape;

pub fn parse_multi_field_match_args(
    args: &[ScalarExpr],
    evaluate: &mut ArgumentEvaluator<'_>,
) -> Result<MultiFieldMatchArgs, SQLError> {
    if args.len() < 3 {
        return Err(SQLError::BadArity {
            name: "multi_field_match".into(),
            expected: ">= 3 (fields..., query[, weights...])".into(),
            actual: args.len(),
        });
    }
    match multi_field_match_shape(args)? {
        MultiFieldMatchShape::FieldsThenQuery {
            fields: field_args,
            query_idx,
        } => {
            let fields = field_args
                .into_iter()
                .map(|arg| expect_column_name(arg, "multi_field_match.field"))
                .collect::<Result<Vec<_>, _>>()?;
            let query = expect_string_value(&args[query_idx], "multi_field_match.query", evaluate)?;
            let weight_args = &args[query_idx + 1..];
            let weights = if weight_args.is_empty() {
                uniform_weights(fields.len())
            } else {
                if weight_args.len() != fields.len() {
                    return Err(SQLError::BadArity {
                        name: "multi_field_match".into(),
                        expected: "one weight per field".into(),
                        actual: weight_args.len(),
                    });
                }
                normalize_weights(
                    weight_args
                        .iter()
                        .map(|arg| expect_f64_value(arg, "multi_field_match.weight", evaluate))
                        .collect::<Result<Vec<_>, _>>()?,
                )?
            };
            let queries = vec![query; fields.len()];
            Ok((fields, queries, weights))
        }
        MultiFieldMatchShape::Pairs { fields: field_args } => {
            let n_fields = field_args.len();
            let mut fields = Vec::with_capacity(n_fields);
            let mut queries = Vec::with_capacity(n_fields);
            for (i, field_arg) in field_args.into_iter().enumerate() {
                fields.push(expect_column_name(field_arg, "multi_field_match.field")?);
                queries.push(expect_string_value(
                    &args[2 * i + 1],
                    "multi_field_match.query",
                    evaluate,
                )?);
            }
            Ok((fields, queries, uniform_weights(n_fields)))
        }
    }
}

fn expect_string_value(
    expr: &ScalarExpr,
    label: &str,
    evaluate: &mut ArgumentEvaluator<'_>,
) -> Result<String, SQLError> {
    match evaluate(expr)? {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be string, got {other:?}"
        ))),
    }
}

pub fn expect_f64_value(
    expr: &ScalarExpr,
    label: &str,
    evaluate: &mut ArgumentEvaluator<'_>,
) -> Result<f64, SQLError> {
    match evaluate(expr)? {
        Value::Float(f) => Ok(f),
        Value::Int(i) => Ok(i as f64),
        Value::Decimal(d) => d
            .to_f64()
            .ok_or_else(|| SQLError::TypeMismatch(format!("{label} decimal is outside f64 range"))),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be numeric, got {other:?}"
        ))),
    }
}

fn uniform_weights(n: usize) -> Vec<f64> {
    vec![1.0 / n.max(1) as f64; n]
}

fn normalize_weights(weights: Vec<f64>) -> Result<Vec<f64>, SQLError> {
    if weights
        .iter()
        .any(|weight| !weight.is_finite() || *weight < 0.0)
    {
        return Err(SQLError::TypeMismatch(
            "multi_field_match weights must be non-negative and finite".into(),
        ));
    }
    let total: f64 = weights.iter().sum();
    if total > 0.0 {
        Ok(weights.into_iter().map(|weight| weight / total).collect())
    } else {
        Err(SQLError::TypeMismatch(
            "multi_field_match weights must have a positive sum".into(),
        ))
    }
}

/// Bind the field before the caller validates its catalog and evaluates the query.
pub fn text_match_field(args: &[ScalarExpr], function_name: &str) -> Result<String, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: function_name.into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    match &args[0] {
        ScalarExpr::Column(name) => Ok(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Ok(column.clone()),
        ScalarExpr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => Ok("_all".into()),
        other => Err(SQLError::TypeMismatch(format!(
            "{function_name}.field must be a column reference, got {other:?}"
        ))),
    }
}

pub struct PriorMatchArguments {
    pub field: String,
    pub prior_field: String,
    pub query: String,
    pub mode: String,
}

pub fn prior_match_arguments(
    args: &[ScalarExpr],
    evaluate: &mut ArgumentEvaluator<'_>,
) -> Result<PriorMatchArguments, SQLError> {
    if args.len() != 4 {
        return Err(SQLError::BadArity {
            name: "bayesian_match_with_prior".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "bayesian_match_with_prior.field")?;
    let prior_field = expect_column_name(&args[2], "bayesian_match_with_prior.prior_field")?;
    let query = expect_string(&args[1], "bayesian_match_with_prior.query", evaluate)?;
    let mode = expect_string(&args[3], "bayesian_match_with_prior.mode", evaluate)?;
    Ok(PriorMatchArguments {
        field,
        prior_field,
        query,
        mode,
    })
}

pub struct CalibratedVectorArguments {
    pub field: String,
    pub query_vector: Vec<f32>,
    pub k: usize,
    pub threshold: Option<f64>,
}

pub fn calibrated_vector_arguments(
    args: &[ScalarExpr],
    evaluate: &mut ArgumentEvaluator<'_>,
) -> Result<CalibratedVectorArguments, SQLError> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "calibrated_vector_match".into(),
            expected: "3..=4".into(),
            actual: args.len(),
        });
    }
    let field = expect_field_name_or_string(&args[0], "calibrated_vector_match.field", evaluate)?;
    let query_vector = crate::expr::value_to_vector(&evaluate(&args[1])?)?;
    let k = expect_usize(&args[2], "calibrated_vector_match.k", evaluate)?;
    let threshold = args
        .get(3)
        .map(|arg| expect_f64_value(arg, "calibrated_vector_match.threshold", evaluate))
        .transpose()?;
    Ok(CalibratedVectorArguments {
        field,
        query_vector,
        k,
        threshold,
    })
}
