//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-field match argument shapes, weights, and execution.

use super::{
    eval_scalar, expect_column_name, validate_text_match_field, Engine, RetrievalExecution,
    SQLError, SQLParam, ScalarEvalContext, ScalarExpr, ScoredEntry, Value,
};

pub(super) fn run_multi_field_match(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    execution: RetrievalExecution,
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() < 3 {
        return Err(SQLError::BadArity {
            name: "multi_field_match".into(),
            expected: ">= 3 (fields..., query[, weights...])".into(),
            actual: args.len(),
        });
    }
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(engine);
    let (fields, queries, weights) = parse_multi_field_match_args(args, &ctx)?;
    for field in &fields {
        validate_text_match_field(engine, table, field, "multi_field_match")?;
    }
    let n_fields = fields.len();
    let mut active_fields = vec![false; n_fields];
    let mut per_doc: std::collections::BTreeMap<u64, Vec<Option<f64>>> =
        std::collections::BTreeMap::new();
    let mut field_priors: Vec<f64> = Vec::new();
    for (i, (field, q)) in fields.iter().zip(queries.iter()).enumerate() {
        let calibration = execution.bayesian_params(engine, table, field)?;
        if calibration.base_rate > 0.0 {
            field_priors.push(calibration.base_rate);
        }
        let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params {
            base_rate: 0.0,
            ..calibration
        });
        let scored = execution.search(engine, table, field, q, &mode, usize::MAX)?;
        for entry in scored {
            active_fields[i] = true;
            let slot = per_doc
                .entry(entry.doc_id)
                .or_insert_with(|| vec![None; n_fields]);
            slot[i] = Some(entry.score);
        }
    }
    let active_field_count = active_fields.iter().filter(|active| **active).count();
    let mut fusion = uqa_fusion::RobustPositiveEvidencePool::new(0.5)
        .map_err(|error| SQLError::TypeMismatch(format!("multi-field fusion: {error}")))?;
    if let Some(base_rate) = crate::operator_tree_bridge::combine_signal_priors(&field_priors) {
        fusion = fusion
            .with_base_rate(base_rate)
            .map_err(|error| SQLError::TypeMismatch(format!("multi-field fusion: {error}")))?;
    }
    let mut out: Vec<ScoredEntry> = per_doc
        .into_iter()
        .map(|(doc_id, probabilities)| -> Result<ScoredEntry, SQLError> {
            let fused = if active_field_count == 1 {
                // A de-facto single field passes through at n = 1,
                // where a configured prior still enters exactly once.
                let evidence = probabilities.into_iter().flatten().next().ok_or_else(|| {
                    SQLError::Internal(format!(
                        "multi-field fusion document {doc_id} has no active signal"
                    ))
                })?;
                fusion.fuse(&[evidence])
            } else {
                fusion
                    .fuse_weighted_sparse(&probabilities, &weights)
                    .map_err(|error| {
                        SQLError::TypeMismatch(format!("multi-field fusion: {error}"))
                    })?
            };
            Ok(ScoredEntry {
                doc_id,
                score: fused,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    out.sort_by_key(|e| e.doc_id);
    Ok(out)
}

type MultiFieldMatchArgs = (Vec<String>, Vec<String>, Vec<f64>);

pub(super) enum MultiFieldMatchShape<'a> {
    FieldsThenQuery {
        fields: Vec<&'a ScalarExpr>,
        query_idx: usize,
    },
    Pairs {
        fields: Vec<&'a ScalarExpr>,
    },
}

pub(super) fn multi_field_match_shape(
    args: &[ScalarExpr],
) -> Result<MultiFieldMatchShape<'_>, SQLError> {
    let first_non_column = args.iter().position(|arg| {
        !matches!(
            arg,
            ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. }
        )
    });
    if let Some(query_idx) = first_non_column {
        if query_idx >= 2 {
            return Ok(MultiFieldMatchShape::FieldsThenQuery {
                fields: args[..query_idx].iter().collect(),
                query_idx,
            });
        }
    }
    if args.len() < 4 || args.len() % 2 != 0 {
        if let Some(query_idx) = first_non_column {
            if query_idx < 2 && args.len() >= 3 {
                return Err(SQLError::TypeMismatch(format!(
                    "multi_field_match field arguments must be column references, \
                     but argument {} is an expression; store computed text in an \
                     indexed column instead of concatenating at query time",
                    query_idx + 1
                )));
            }
        }
        return Err(SQLError::BadArity {
            name: "multi_field_match".into(),
            expected: ">= 3 (fields..., query[, weights...]) or even >= 4 (field, query pairs)"
                .into(),
            actual: args.len(),
        });
    }
    Ok(MultiFieldMatchShape::Pairs {
        fields: (0..args.len() / 2).map(|i| &args[2 * i]).collect(),
    })
}

fn parse_multi_field_match_args(
    args: &[ScalarExpr],
    ctx: &ScalarEvalContext<'_>,
) -> Result<MultiFieldMatchArgs, SQLError> {
    match multi_field_match_shape(args)? {
        MultiFieldMatchShape::FieldsThenQuery {
            fields: field_args,
            query_idx,
        } => {
            let fields = field_args
                .into_iter()
                .map(|arg| expect_column_name(arg, "multi_field_match.field"))
                .collect::<Result<Vec<_>, _>>()?;
            let query = expect_string_value(&args[query_idx], "multi_field_match.query", ctx)?;
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
                        .map(|arg| expect_f64_value(arg, "multi_field_match.weight", ctx))
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
                    ctx,
                )?);
            }
            Ok((fields, queries, uniform_weights(n_fields)))
        }
    }
}

fn expect_string_value(
    expr: &ScalarExpr,
    label: &str,
    ctx: &ScalarEvalContext<'_>,
) -> Result<String, SQLError> {
    match eval_scalar(expr, ctx)? {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be string, got {other:?}"
        ))),
    }
}

pub(super) fn expect_f64_value(
    expr: &ScalarExpr,
    label: &str,
    ctx: &ScalarEvalContext<'_>,
) -> Result<f64, SQLError> {
    match eval_scalar(expr, ctx)? {
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
