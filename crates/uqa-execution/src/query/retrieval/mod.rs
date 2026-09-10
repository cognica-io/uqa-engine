//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text, prior-aware, multi-field and calibrated-vector retrieval execution.

use crate::{eval_scalar, ScalarEvalContext};
use context::{RetrievalDocuments, TextRetrievalContext, VectorPoolRetrieval};
use std::collections::BTreeMap;
use uqa_core::{DocId, ScoredEntry, Value};
use uqa_sql::semantics::text_indexes::{validate_text_match_all_fields, validate_text_match_field};
use uqa_sql::{SQLError, SQLParam, ScalarExpr};

pub mod context;
mod multi_field;
pub use multi_field::run_multi_field_match;

fn run_bayesian_match(
    context: &TextRetrievalContext<'_>,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    top_k: Option<usize>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_text_match_scored(
        context,
        table,
        args,
        params,
        TextMatchExecution {
            function_name: "bayesian_match",
            mode_for_field: &|field| {
                Ok(uqa_scoring::ScoringMode::BayesianBM25(
                    context.text.bayesian_params(table, field)?,
                ))
            },
            top_k,
        },
    )
}

struct TextMatchExecution<'a> {
    function_name: &'a str,
    mode_for_field: &'a dyn Fn(&str) -> Result<uqa_scoring::ScoringMode, SQLError>,
    top_k: Option<usize>,
}

fn run_text_match_scored(
    context: &TextRetrievalContext<'_>,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    execution: TextMatchExecution<'_>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let function_name = execution.function_name;
    let field = uqa_sql::semantics::retrieval::text_match_field(args, function_name)?;
    if field == "_all" || field.is_empty() {
        validate_text_match_all_fields(context.catalog, table, function_name)?;
    } else {
        validate_text_match_field(context.catalog, table, &field, function_name)?;
    }
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(context.functions);
    let query_value = eval_scalar(&args[1], &ctx)?;
    let query = match query_value {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name} query must be a string, got {other:?}"
            )));
        }
    };
    if field == "_all" || field.is_empty() {
        let mut by_doc: BTreeMap<DocId, f64> = BTreeMap::new();
        for field_name in context.catalog.indexed_fields(table)? {
            let mode = (execution.mode_for_field)(&field_name)?;
            for entry in context
                .text
                .search(table, &field_name, &query, &mode, usize::MAX)?
            {
                by_doc
                    .entry(entry.doc_id)
                    .and_modify(|score| *score = (*score).max(entry.score))
                    .or_insert(entry.score);
            }
        }
        return Ok(by_doc
            .into_iter()
            .map(|(doc_id, score)| ScoredEntry { doc_id, score })
            .collect());
    }
    let mode = (execution.mode_for_field)(&field)?;
    context.text.search(
        table,
        &field,
        &query,
        &mode,
        execution.top_k.unwrap_or(usize::MAX),
    )
}

pub fn run_bayesian_match_with_prior(
    documents: &dyn RetrievalDocuments,
    context: &TextRetrievalContext<'_>,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(context.functions);
    let uqa_sql::semantics::retrieval::PriorMatchArguments {
        field,
        prior_field,
        query,
        mode,
    } = uqa_sql::semantics::retrieval::prior_match_arguments(args, &mut |expr| {
        eval_scalar(expr, &ctx)
    })?;

    let base = run_bayesian_match(
        context,
        table,
        &[
            ScalarExpr::Column(field),
            ScalarExpr::Literal(Value::Str(query)),
        ],
        params,
        None,
    )?;
    let prior_fn = prior_fn_for_mode(&mode, &prior_field)?;
    let mut scored = Vec::with_capacity(base.len());
    for entry in base {
        let document = documents
            .get_document(table, entry.doc_id)?
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "bayesian prior: posting references missing document {} in table `{table}`",
                    entry.doc_id
                ))
            })?;
        let prior = prior_fn(&document).clamp(1e-10, 1.0 - 1e-10);
        scored.push(ScoredEntry {
            doc_id: entry.doc_id,
            score: combine_probability_with_prior(entry.score, prior),
        });
    }
    Ok(scored)
}

fn prior_fn_for_mode(mode: &str, prior_field: &str) -> Result<uqa_scoring::PriorFn, SQLError> {
    match mode.to_ascii_lowercase().as_str() {
        "authority" => Ok(uqa_scoring::authority_prior(prior_field, None)),
        "recency" => Ok(uqa_scoring::recency_prior(prior_field, 30.0)),
        other => Err(SQLError::TypeMismatch(format!(
            "Unknown prior mode: {other}"
        ))),
    }
}

fn combine_probability_with_prior(probability: f64, prior: f64) -> f64 {
    let p = probability.clamp(1e-10, 1.0 - 1e-10);
    uqa_scoring::sigmoid(uqa_scoring::logit(p) + uqa_scoring::logit(prior))
}

pub fn run_calibrated_vector_match(
    vector: &dyn VectorPoolRetrieval,
    functions: &dyn uqa_sql::expr::EngineHook,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(functions);
    let uqa_sql::semantics::retrieval::CalibratedVectorArguments {
        field,
        query_vector,
        k,
        threshold,
    } = uqa_sql::semantics::retrieval::calibrated_vector_arguments(args, &mut |expr| {
        eval_scalar(expr, &ctx)
    })?;
    let mut out = vector.query_pool(table, &field, &query_vector, k)?;
    out.retain(|entry| threshold.is_none_or(|minimum| entry.score >= minimum));
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    });
    Ok(out)
}

/// Combine signal corpus priors by averaging their logits so the corpus prior enters fusion once.
pub fn combine_signal_priors(priors: &[f64]) -> Option<f64> {
    if priors.is_empty() {
        return None;
    }
    let mean_logit = priors
        .iter()
        .map(|rate| uqa_scoring::logit(*rate))
        .sum::<f64>()
        / priors.len() as f64;
    Some(uqa_scoring::sigmoid(mean_logit))
}
