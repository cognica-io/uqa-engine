//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evidence-pool, Bayesian, attention, and learned-fusion lowering.

use super::{
    const_bool, const_f64, const_f64_vector, const_gating, const_usize, lower_signal_arg,
    named_arg_expr, BTreeSet, DriverResult, GatingSpec, OperatorTree, SQLError, SQLParam,
    ScalarExpr,
};

pub(super) fn lower_positive_evidence_pool(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    // `pool_positive_evidence(signal_1, signal_2, ...[, alpha[, gating]])`.
    // The UQA SQL contract defaults alpha to 0.5 when no numeric option is supplied;
    // don't treat the last signal as an alpha argument.
    if args.len() < 2 {
        return None;
    }

    let mut alpha = 0.5;
    let mut gating = GatingSpec::Softplus;
    let mut weights = None;
    let mut logit_min = None;
    let mut logit_max = None;
    let mut signal_end = args.len();
    while signal_end > 0 {
        let option = &args[signal_end - 1];
        if let Some((name, value_expr)) = named_arg_expr(option) {
            if name.eq_ignore_ascii_case("alpha") {
                alpha = const_f64(value_expr, params)?;
            } else if name.eq_ignore_ascii_case("gating") {
                gating = const_gating(value_expr, params)?;
            } else if name.eq_ignore_ascii_case("weights") {
                weights = Some(const_f64_vector(value_expr, params)?);
            } else if name.eq_ignore_ascii_case("logit_min") {
                logit_min = Some(const_f64_vector(value_expr, params)?);
            } else if name.eq_ignore_ascii_case("logit_max") {
                logit_max = Some(const_f64_vector(value_expr, params)?);
            } else {
                return None;
            }
            signal_end -= 1;
            continue;
        }
        if let Some(g) = const_gating(option, params) {
            gating = g;
            signal_end -= 1;
            continue;
        }
        if let Some(v) = const_f64(option, params) {
            alpha = v;
            signal_end -= 1;
            continue;
        }
        break;
    }
    if signal_end < 2 {
        return None;
    }
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return None;
    }
    if let Some(signal_weights) = &weights {
        let sum = signal_weights.iter().sum::<f64>();
        if signal_weights.len() != signal_end
            || signal_weights
                .iter()
                .any(|weight| !weight.is_finite() || *weight < 0.0)
            || (sum - 1.0).abs() > 1e-3
        {
            return None;
        }
    }
    match (&logit_min, &logit_max) {
        (Some(minimums), Some(maximums))
            if minimums.len() == signal_end && maximums.len() == signal_end => {}
        (Some(_), Some(_)) => return None,
        _ => {
            logit_min = None;
            logit_max = None;
        }
    }

    let mut signals: Vec<OperatorTree> = Vec::with_capacity(signal_end);
    for a in &args[..signal_end] {
        signals.push(lower_signal_arg(a, params)?);
    }
    Some(OperatorTree::RobustPositiveEvidencePool {
        signals,
        alpha,
        gating,
        weights,
        logit_min,
        logit_max,
        adaptive_weights: false,
    })
}

pub(super) fn lower_bayesian_evidence_fusion(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    if args.len() < 2 {
        return None;
    }
    let mut signal_end = args.len();
    let mut base_rate = None;
    if let Some((name, value_expr)) = named_arg_expr(args.last()?) {
        if !name.eq_ignore_ascii_case("base_rate") {
            return None;
        }
        let value = const_f64(value_expr, params)?;
        if !value.is_finite() || value <= 0.0 || value >= 1.0 {
            return None;
        }
        base_rate = Some(value);
        signal_end -= 1;
    }
    if signal_end < 2 {
        return None;
    }
    let signals = args[..signal_end]
        .iter()
        .map(|argument| lower_signal_arg(argument, params))
        .collect::<Option<Vec<_>>>()?;
    Some(OperatorTree::BayesianEvidenceFusion { signals, base_rate })
}

struct AttentionLoweringOptions<'a> {
    signal_args: Vec<&'a ScalarExpr>,
    alpha: f64,
    normalized: bool,
    base_rate: Option<f64>,
    n_heads: usize,
    multi_head: bool,
}

fn parse_attention_options<'a>(
    function_name: &str,
    args: &'a [ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<AttentionLoweringOptions<'a>> {
    let multi_head = function_name.eq_ignore_ascii_case("fuse_multihead");
    let valid_options: &[&str] = if multi_head {
        &["n_heads", "normalized", "alpha"]
    } else {
        &["normalized", "alpha", "base_rate"]
    };
    let mut options = AttentionLoweringOptions {
        signal_args: Vec::new(),
        alpha: 0.5,
        normalized: false,
        base_rate: None,
        n_heads: 4,
        multi_head,
    };
    let mut seen_options = BTreeSet::new();
    let mut saw_option = false;

    for argument in args {
        if let Some((option_name, value)) = named_arg_expr(argument) {
            saw_option = true;
            let option_name = option_name.to_ascii_lowercase();
            if !valid_options.contains(&option_name.as_str()) {
                return Err(SQLError::TypeMismatch(format!(
                    "unknown option `{option_name}` for {function_name}; valid options: {}",
                    valid_options.join(", ")
                )));
            }
            if !seen_options.insert(option_name.clone()) {
                return Err(SQLError::TypeMismatch(format!(
                    "duplicate option `{option_name}` for {function_name}"
                )));
            }
            match option_name.as_str() {
                "alpha" => {
                    options.alpha = const_f64(value, params).ok_or_else(|| {
                        SQLError::TypeMismatch(format!(
                            "{function_name}.alpha must be a constant number"
                        ))
                    })?;
                }
                "normalized" => {
                    options.normalized = const_bool(value, params).ok_or_else(|| {
                        SQLError::TypeMismatch(format!(
                            "{function_name}.normalized must be a constant boolean"
                        ))
                    })?;
                }
                "base_rate" => {
                    options.base_rate = Some(const_f64(value, params).ok_or_else(|| {
                        SQLError::TypeMismatch(format!(
                            "{function_name}.base_rate must be a constant number"
                        ))
                    })?);
                }
                "n_heads" => {
                    options.n_heads = const_usize(value, params).ok_or_else(|| {
                        SQLError::TypeMismatch(format!(
                            "{function_name}.n_heads must be a constant non-negative integer"
                        ))
                    })?;
                }
                _ => unreachable!("valid attention option was matched above"),
            }
        } else {
            if uqa_execution::scalar_call_argument(argument)
                .ok()
                .is_some_and(|argument| argument.name.is_some())
            {
                return Err(SQLError::TypeMismatch(format!(
                    "malformed named option for {function_name}"
                )));
            }
            if saw_option {
                return Err(SQLError::TypeMismatch(format!(
                    "{function_name} signal arguments must precede named options"
                )));
            }
            options.signal_args.push(argument);
        }
    }

    validate_attention_options(function_name, &options)?;
    Ok(options)
}

fn validate_attention_options(
    function_name: &str,
    options: &AttentionLoweringOptions<'_>,
) -> DriverResult<()> {
    if options.signal_args.len() < 2 {
        return Err(SQLError::BadArity {
            name: function_name.to_string(),
            expected: ">=2 signals".to_string(),
            actual: options.signal_args.len(),
        });
    }
    if !options.alpha.is_finite() || !(0.0..=1.0).contains(&options.alpha) {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}.alpha must be finite and in [0, 1], got {}",
            options.alpha
        )));
    }
    if options
        .base_rate
        .is_some_and(|rate| !rate.is_finite() || rate <= 0.0 || rate >= 1.0)
    {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}.base_rate must be finite and in (0, 1), got {}",
            options.base_rate.expect("checked Some above")
        )));
    }
    if options.multi_head && options.n_heads == 0 {
        return Err(SQLError::TypeMismatch(
            "fuse_multihead.n_heads must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn try_lower_attention_fusion(
    function_name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<OperatorTree> {
    use std::sync::Arc;
    use uqa_fusion::{AttentionFusion, MultiHeadAttentionFusion, N_QUERY_FEATURES};
    use uqa_operators::tree::AttentionRef;

    let options = parse_attention_options(function_name, args, params)?;

    let mut signals = Vec::with_capacity(options.signal_args.len());
    for (index, argument) in options.signal_args.into_iter().enumerate() {
        signals.push(lower_signal_arg(argument, params).ok_or_else(|| {
            SQLError::TypeMismatch(format!(
                "{function_name} signal {} cannot be lowered to a probability-valued operator",
                index + 1
            ))
        })?);
    }

    let attention: AttentionRef = if options.multi_head {
        Arc::new(
            MultiHeadAttentionFusion::try_new(
                options.n_heads,
                signals.len(),
                N_QUERY_FEATURES,
                options.alpha,
                options.normalized,
            )
            .map_err(|error| SQLError::TypeMismatch(format!("{function_name}: {error}")))?,
        )
    } else {
        Arc::new(
            AttentionFusion::new(signals.len(), N_QUERY_FEATURES, options.alpha)
                .with_options(options.normalized, options.base_rate)
                .map_err(|error| SQLError::TypeMismatch(format!("{function_name}: {error}")))?,
        )
    };

    // Query features are filled in lazily at execute time from the engine
    // snapshot, so the IR carries an empty explicit vector.
    Ok(OperatorTree::AttentionFusion {
        signals,
        attention,
        query_features: Vec::new(),
    })
}

pub(super) fn lower_learned_fusion(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    use std::sync::Arc;
    use uqa_fusion::LearnedFusion;
    use uqa_operators::tree::LearnedFusionRef;

    let mut signal_end = args.len();
    let mut alpha = 0.5;
    if let Some((name, value)) = args.last().and_then(named_arg_expr) {
        if !name.eq_ignore_ascii_case("alpha") {
            return None;
        }
        alpha = const_f64(value, params)?;
        signal_end -= 1;
    }
    if signal_end < 2 || !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return None;
    }

    let mut signals: Vec<OperatorTree> = Vec::with_capacity(signal_end);
    for a in &args[..signal_end] {
        signals.push(lower_signal_arg(a, params)?);
    }
    let learned: LearnedFusionRef = Arc::new(LearnedFusion::new(signals.len(), alpha));
    Some(OperatorTree::LearnedFusion { signals, learned })
}
