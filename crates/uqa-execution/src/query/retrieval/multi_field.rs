//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-field search, sparse signal padding, and evidence fusion.

use super::{
    eval_scalar, validate_text_match_field, SQLError, SQLParam, ScalarEvalContext, ScalarExpr,
    ScoredEntry, TextRetrievalContext,
};

pub fn run_multi_field_match(
    context: &TextRetrievalContext<'_>,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(context.functions);
    let (fields, queries, weights) =
        uqa_sql::semantics::retrieval::parse_multi_field_match_args(args, &mut |expr| {
            eval_scalar(expr, &ctx)
        })?;
    for field in &fields {
        validate_text_match_field(context.catalog, table, field, "multi_field_match")?;
    }
    let n_fields = fields.len();
    let mut active_fields = vec![false; n_fields];
    let mut per_doc: std::collections::BTreeMap<u64, Vec<Option<f64>>> =
        std::collections::BTreeMap::new();
    let mut field_priors: Vec<f64> = Vec::new();
    for (i, (field, q)) in fields.iter().zip(queries.iter()).enumerate() {
        let calibration = context.text.bayesian_params(table, field)?;
        if calibration.base_rate > 0.0 {
            field_priors.push(calibration.base_rate);
        }
        let mode = uqa_scoring::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params {
            base_rate: 0.0,
            ..calibration
        });
        let scored = context.text.search(table, field, q, &mode, usize::MAX)?;
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
    if let Some(base_rate) = super::combine_signal_priors(&field_priors) {
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
