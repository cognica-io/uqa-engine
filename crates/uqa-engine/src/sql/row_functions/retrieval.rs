//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bayesian text, prior-aware, and calibrated-vector retrieval paths.

use super::{
    eval_scalar, expect_column_name, expect_f64_value, expect_field_name_or_string, expect_string,
    expect_usize, run_multi_field_match, validate_text_match_all_fields, validate_text_match_field,
    value_to_vector, BTreeMap, DocId, Engine, RetrievalExecution, SQLError, SQLParam,
    ScalarEvalContext, ScalarExpr, ScoredEntry, Value,
};

pub(crate) fn run_bayesian_match_with_prior_public(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_bayesian_match_with_prior(engine, table, args, params, RetrievalExecution::Public)
}

pub(crate) fn run_bayesian_match_with_prior_in_execution(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_bayesian_match_with_prior(engine, table, args, params, RetrievalExecution::InExecution)
}

pub(crate) fn run_calibrated_vector_match_public(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_calibrated_vector_match(engine, table, args, params)
}

pub(crate) fn run_multi_field_match_public(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_multi_field_match(engine, table, args, params, RetrievalExecution::Public)
}

pub(crate) fn run_multi_field_match_in_execution(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_multi_field_match(engine, table, args, params, RetrievalExecution::InExecution)
}

fn run_bayesian_match(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    top_k: Option<usize>,
    execution: RetrievalExecution,
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_text_match_scored(
        engine,
        table,
        args,
        params,
        TextMatchExecution {
            function_name: "bayesian_match",
            mode_for_field: &|field| {
                Ok(crate::ScoringMode::BayesianBM25(
                    execution.bayesian_params(engine, table, field)?,
                ))
            },
            top_k,
            retrieval: execution,
        },
    )
}

/// `bayesian_match` with the corpus prior stripped: emits prior-free
/// evidence probabilities for fusion contexts, where the prior enters
/// the fusion exactly once instead of once per signal.
fn run_bayesian_evidence_match(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    execution: RetrievalExecution,
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_text_match_scored(
        engine,
        table,
        args,
        params,
        TextMatchExecution {
            function_name: "bayesian_match",
            mode_for_field: &|field| {
                Ok(crate::ScoringMode::BayesianBM25(
                    execution
                        .bayesian_params(engine, table, field)?
                        .evidence_params(),
                ))
            },
            top_k: None,
            retrieval: execution,
        },
    )
}

pub(crate) fn run_bayesian_evidence_match_public(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_bayesian_evidence_match(engine, table, args, params, RetrievalExecution::Public)
}

pub(crate) fn run_bayesian_evidence_match_in_execution(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_bayesian_evidence_match(engine, table, args, params, RetrievalExecution::InExecution)
}

struct TextMatchExecution<'a> {
    function_name: &'a str,
    mode_for_field: &'a dyn Fn(&str) -> Result<crate::ScoringMode, SQLError>,
    top_k: Option<usize>,
    retrieval: RetrievalExecution,
}

fn run_text_match_scored(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    execution: TextMatchExecution<'_>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let function_name = execution.function_name;
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: function_name.into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let field = match &args[0] {
        ScalarExpr::Column(name) => name.clone(),
        ScalarExpr::QualifiedColumn { column, .. } => column.clone(),
        ScalarExpr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => "_all".to_string(),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name}.field must be a column reference, got {other:?}"
            )));
        }
    };
    if field == "_all" || field.is_empty() {
        validate_text_match_all_fields(engine, table, function_name)?;
    } else {
        validate_text_match_field(engine, table, &field, function_name)?;
    }
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(engine);
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
        for field_name in engine.fts_fields_for_table(table)? {
            let mode = (execution.mode_for_field)(&field_name)?;
            for entry in
                execution
                    .retrieval
                    .search(engine, table, &field_name, &query, &mode, usize::MAX)?
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
    execution.retrieval.search(
        engine,
        table,
        &field,
        &query,
        &mode,
        execution.top_k.unwrap_or(usize::MAX),
    )
}

fn run_bayesian_match_with_prior(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    execution: RetrievalExecution,
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 4 {
        return Err(SQLError::BadArity {
            name: "bayesian_match_with_prior".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "bayesian_match_with_prior.field")?;
    let prior_field = expect_column_name(&args[2], "bayesian_match_with_prior.prior_field")?;
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(engine);
    let query = expect_string(&args[1], "bayesian_match_with_prior.query", &ctx)?;
    let mode = expect_string(&args[3], "bayesian_match_with_prior.mode", &ctx)?;

    let base = run_bayesian_match(
        engine,
        table,
        &[
            ScalarExpr::Column(field),
            ScalarExpr::Literal(Value::Str(query)),
        ],
        params,
        None,
        execution,
    )?;
    let prior_fn = prior_fn_for_mode(&mode, &prior_field)?;
    let mut scored = Vec::with_capacity(base.len());
    for entry in base {
        let document = engine.get_document(table, entry.doc_id)?.ok_or_else(|| {
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

fn run_calibrated_vector_match(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "calibrated_vector_match".into(),
            expected: "3..=4".into(),
            actual: args.len(),
        });
    }
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(engine);
    let field = expect_field_name_or_string(&args[0], "calibrated_vector_match.field", &ctx)?;
    let query_vector = value_to_vector(&eval_scalar(&args[1], &ctx)?)?;
    let k = expect_usize(&args[2], "calibrated_vector_match.k", &ctx)?;
    let threshold = if let Some(arg) = args.get(3) {
        Some(expect_f64_value(
            arg,
            "calibrated_vector_match.threshold",
            &ctx,
        )?)
    } else {
        None
    };
    let Some(ctx) = engine.snapshot_context(table)? else {
        return Err(SQLError::UnknownTable(table.to_string()));
    };
    use uqa_operators::base::Operator;
    let op = uqa_operators::QueryPoolVectorScoreOperator::new(query_vector, k, field);
    let pl = op
        .execute(&ctx)
        .map_err(|error| SQLError::Internal(format!("calibrated vector search: {error}")))?;
    let mut out: Vec<ScoredEntry> = pl
        .iter()
        .filter_map(|entry| {
            let score = entry.payload.score;
            if threshold.is_some_and(|t| score < t) {
                return None;
            }
            Some(ScoredEntry {
                doc_id: entry.doc_id,
                score,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    });
    Ok(out)
}
