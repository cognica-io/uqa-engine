//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text, vector, and staged-retrieval argument validation and lowering.

mod fts;

use super::{
    column_name, const_f64, const_string, const_usize, const_vector, eval_scalar, lower_function,
    named_arg_expr, BTreeSet, DriverResult, Engine, ExternalPriorMode, MultiStageCutoff,
    MultiStageEntry, OperatorTree, SQLError, SQLParam, ScalarEvalContext, ScalarExpr,
    TextScoringMode, Value,
};

pub(super) fn try_lower_checked_retrieval(
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<DriverResult<OperatorTree>> {
    match name.to_ascii_lowercase().as_str() {
        "text_match" => Some(try_lower_text_match(
            "text_match",
            args,
            params,
            TextScoringMode::BM25,
        )),
        "bayesian_match" => Some(try_lower_text_match(
            "bayesian_match",
            args,
            params,
            TextScoringMode::BayesianBM25,
        )),
        "fts_match" => Some(try_lower_fts_match(args, params)),
        "bayesian_match_with_prior" => Some(try_lower_bayesian_match_with_prior(args, params)),
        "knn_match" => Some(try_lower_knn_match(args, params)),
        "calibrated_vector_match" => Some(try_lower_calibrated_vector_match(args, params)),
        _ => None,
    }
}

pub(super) fn validate_checked_retrieval_call_tree(
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<()> {
    if let Some(result) = try_lower_checked_retrieval(name, args, params) {
        result?;
    }
    for argument in args {
        if let ScalarExpr::Func {
            name: child_name,
            args: child_args,
            ..
        } = argument
        {
            validate_checked_retrieval_call_tree(child_name, child_args, params)?;
        }
    }
    Ok(())
}

pub(super) fn checked_retrieval_call_tree_present(name: &str, args: &[ScalarExpr]) -> bool {
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "text_match"
            | "bayesian_match"
            | "fts_match"
            | "bayesian_match_with_prior"
            | "knn_match"
            | "calibrated_vector_match"
    ) {
        return true;
    }
    args.iter().any(|argument| {
        let ScalarExpr::Func {
            name: child_name,
            args: child_args,
            ..
        } = argument
        else {
            return false;
        };
        checked_retrieval_call_tree_present(child_name, child_args)
    })
}

pub(super) fn bind_operator_argument(
    engine: &Engine,
    expression: &ScalarExpr,
    params: &[SQLParam],
) -> DriverResult<ScalarExpr> {
    match expression {
        ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. } => Ok(expression.clone()),
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } if uqa_execution::scalar_call_argument(expression)
            .ok()
            .is_some_and(|argument| argument.name.is_some())
            || uqa_sql::registry::lookup(name).is_some() =>
        {
            if *distinct || !order_by.is_empty() || filter.is_some() {
                return Err(SQLError::TypeMismatch(format!(
                    "operator function `{name}` does not accept aggregate modifiers"
                )));
            }
            Ok(ScalarExpr::Func {
                name: name.clone(),
                binding: binding.clone(),
                args: args
                    .iter()
                    .map(|argument| bind_operator_argument(engine, argument, params))
                    .collect::<Result<Vec<_>, _>>()?,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        other => {
            let context = ScalarEvalContext::new(None, params).with_function_hook(engine);
            eval_scalar(other, &context).map(ScalarExpr::Literal)
        }
    }
}

pub(super) fn validate_operator_function_arity(name: &str, actual: usize) -> DriverResult<()> {
    let lower = name.to_ascii_lowercase();
    let expected = match lower.as_str() {
        "text_match" | "bayesian_match" | "fts_match" | "sparse_threshold" => {
            (actual != 2).then_some("2")
        }
        "bayesian_match_with_prior" | "graph_traverse" | "traverse_match" | "graph_neighbors" => {
            (actual != 4).then_some("4")
        }
        "knn_match" => (actual != 3).then_some("3"),
        "rpq" => (!(2..=3).contains(&actual)).then_some("2..=3"),
        "calibrated_vector_match" => (!(3..=4).contains(&actual)).then_some("3..=4"),
        "graph_edges" => (!(1..=2).contains(&actual)).then_some("1..=2"),
        "temporal_traverse" => (actual != 6).then_some("6"),
        "deep_predict" => (actual != 1).then_some("1"),
        "graph_pagerank" | "pagerank" | "graph_hits" | "hits" | "graph_betweenness"
        | "betweenness" => (actual > 1).then_some("0..=1"),
        "fuse_bayesian_evidence"
        | "pool_positive_evidence"
        | "fuse_log_odds"
        | "attention"
        | "fuse_attention"
        | "fuse_multihead"
        | "learned_fusion"
        | "fuse_learned"
        | "staged_retrieval" => (actual < 2).then_some(">=2"),
        "multi_field_match" => (actual < 3).then_some(">=3"),
        _ => None,
    };
    if let Some(expected) = expected {
        return Err(SQLError::BadArity {
            name: lower,
            expected: expected.into(),
            actual,
        });
    }
    Ok(())
}

pub(super) fn validate_probability_signal_contract(
    name: &str,
    args: &[ScalarExpr],
) -> DriverResult<()> {
    if !matches!(
        name.to_ascii_lowercase().as_str(),
        "fuse_bayesian_evidence"
            | "pool_positive_evidence"
            | "fuse_log_odds"
            | "attention"
            | "fuse_attention"
            | "fuse_multihead"
            | "learned_fusion"
            | "fuse_learned"
    ) {
        return Ok(());
    }
    if args.iter().any(|argument| {
        matches!(
            argument,
            ScalarExpr::Func { name, .. } if name.eq_ignore_ascii_case("text_match")
        )
    }) {
        return Err(SQLError::TypeMismatch(format!(
            "{name} requires probability-valued signals; text_match returns raw BM25 scores, use bayesian_match instead"
        )));
    }
    Ok(())
}

pub(super) fn bad_operator_arity(name: &str, expected: &str, actual: usize) -> SQLError {
    SQLError::BadArity {
        name: name.to_string(),
        expected: expected.to_string(),
        actual,
    }
}

pub(super) fn try_lower_text_match(
    function_name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    scoring: TextScoringMode,
) -> DriverResult<OperatorTree> {
    if args.len() != 2 {
        return Err(bad_operator_arity(function_name, "2", args.len()));
    }
    let field = match &args[0] {
        ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. }
            if name.is_empty() || name == "_all" =>
        {
            None
        }
        ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. } => {
            Some(name.clone())
        }
        ScalarExpr::Literal(Value::Str(name)) if name.is_empty() || name == "_all" => None,
        _ => {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name}.field must be a column reference, '_all', or an empty string"
            )))
        }
    };
    let query = const_string(&args[1], params).ok_or_else(|| {
        SQLError::TypeMismatch(format!("{function_name}.query must be a constant string"))
    })?;
    Ok(OperatorTree::Term {
        query,
        field,
        scoring: Some(scoring),
        top_k: None,
    })
}

pub(super) fn try_lower_fts_match(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<OperatorTree> {
    const FUNCTION_NAME: &str = "fts_match";
    if args.len() != 2 {
        return Err(bad_operator_arity(FUNCTION_NAME, "2", args.len()));
    }
    let default_field = fts_default_field(&args[0]).ok_or_else(|| {
        SQLError::TypeMismatch(
            "fts_match.field must be a column reference, '_all', or an empty string".into(),
        )
    })?;
    let query = const_string(&args[1], params).ok_or_else(|| {
        SQLError::TypeMismatch("fts_match.query must be a constant string".into())
    })?;
    let tree = fts::compile_query_string(&query, default_field.as_deref())
        .map_err(|error| SQLError::TypeMismatch(format!("fts_match.query: {error}")))?;
    Ok(prepare_fts_probability_tree(tree))
}

pub(super) fn try_lower_bayesian_match_with_prior(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<OperatorTree> {
    const FUNCTION_NAME: &str = "bayesian_match_with_prior";
    if args.len() != 4 {
        return Err(bad_operator_arity(FUNCTION_NAME, "4", args.len()));
    }
    let field = column_name(&args[0]).ok_or_else(|| {
        SQLError::TypeMismatch("bayesian_match_with_prior.field must be a column reference".into())
    })?;
    let query = const_string(&args[1], params).ok_or_else(|| {
        SQLError::TypeMismatch("bayesian_match_with_prior.query must be a constant string".into())
    })?;
    let prior_field = column_name(&args[2]).ok_or_else(|| {
        SQLError::TypeMismatch(
            "bayesian_match_with_prior.prior_field must be a column reference".into(),
        )
    })?;
    let mode_name = const_string(&args[3], params).ok_or_else(|| {
        SQLError::TypeMismatch("bayesian_match_with_prior.mode must be a constant string".into())
    })?;
    let mode = match mode_name.to_ascii_lowercase().as_str() {
        "authority" => ExternalPriorMode::Authority,
        "recency" => ExternalPriorMode::Recency,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "Unknown prior mode: {other}"
            )))
        }
    };
    Ok(OperatorTree::BayesianMatchWithPrior {
        field,
        query,
        prior_field,
        mode,
    })
}

pub(super) fn lower_bayesian_match_with_prior(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    try_lower_bayesian_match_with_prior(args, params).ok()
}

pub(super) fn try_lower_knn_match(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<OperatorTree> {
    const FUNCTION_NAME: &str = "knn_match";
    if args.len() != 3 {
        return Err(bad_operator_arity(FUNCTION_NAME, "3", args.len()));
    }
    let field = column_name(&args[0]).ok_or_else(|| {
        SQLError::TypeMismatch("knn_match.field must be a column reference".into())
    })?;
    if field.trim().is_empty() {
        return Err(SQLError::TypeMismatch(
            "knn_match.field cannot be empty".into(),
        ));
    }
    let query_vector = const_vector(&args[1], params).ok_or_else(|| {
        SQLError::TypeMismatch("knn_match.vector must be a constant numeric vector".into())
    })?;
    if query_vector.is_empty() || query_vector.iter().any(|component| !component.is_finite()) {
        return Err(SQLError::TypeMismatch(
            "knn_match.vector must be non-empty and contain only finite values".into(),
        ));
    }
    let k = const_usize(&args[2], params).ok_or_else(|| {
        SQLError::TypeMismatch("knn_match.k must be a non-negative integer".into())
    })?;
    if k == 0 || i64::try_from(k).is_err() {
        return Err(SQLError::TypeMismatch(format!(
            "knn_match.k must be positive and fit in a SQL BIGINT, got {k}"
        )));
    }
    Ok(OperatorTree::KNN {
        query_vector,
        k,
        field,
    })
}

pub(super) fn try_lower_calibrated_vector_match(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<OperatorTree> {
    const FUNCTION_NAME: &str = "calibrated_vector_match";
    if !(3..=4).contains(&args.len()) {
        return Err(bad_operator_arity(FUNCTION_NAME, "3..=4", args.len()));
    }
    let field = field_name_arg(&args[0], params).ok_or_else(|| {
        SQLError::TypeMismatch(
            "calibrated_vector_match.field must be a column reference or constant string".into(),
        )
    })?;
    if field.trim().is_empty() {
        return Err(SQLError::TypeMismatch(
            "calibrated_vector_match.field cannot be empty".into(),
        ));
    }
    let query_vector = const_vector(&args[1], params).ok_or_else(|| {
        SQLError::TypeMismatch(
            "calibrated_vector_match.vector must be a constant numeric vector".into(),
        )
    })?;
    if query_vector.is_empty() || query_vector.iter().any(|component| !component.is_finite()) {
        return Err(SQLError::TypeMismatch(
            "calibrated_vector_match.vector must be non-empty and contain only finite values"
                .into(),
        ));
    }
    let k = const_usize(&args[2], params).ok_or_else(|| {
        SQLError::TypeMismatch("calibrated_vector_match.k must be a non-negative integer".into())
    })?;
    if k == 0 || i64::try_from(k).is_err() {
        return Err(SQLError::TypeMismatch(format!(
            "calibrated_vector_match.k must be positive and fit in a SQL BIGINT, got {k}"
        )));
    }
    let threshold = args
        .get(3)
        .map(|argument| {
            const_f64(argument, params).ok_or_else(|| {
                SQLError::TypeMismatch(
                    "calibrated_vector_match.threshold must be a constant number".into(),
                )
            })
        })
        .transpose()?;
    if threshold.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
        return Err(SQLError::TypeMismatch(format!(
            "calibrated_vector_match.threshold must be finite and in [0, 1], got {}",
            threshold.expect("checked Some above")
        )));
    }
    Ok(OperatorTree::CalibratedVectorMatch {
        field,
        query_vector,
        k,
        threshold,
    })
}

pub(super) fn lower_calibrated_vector_match(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    try_lower_calibrated_vector_match(args, params).ok()
}

pub(super) fn lower_multi_field_match(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    if args.len() < 3 {
        return None;
    }
    let first_non_column = args.iter().position(|arg| column_name(arg).is_none());
    if let Some(query_idx) = first_non_column {
        if query_idx >= 2 {
            let fields = args[..query_idx]
                .iter()
                .map(column_name)
                .collect::<Option<Vec<_>>>()?;
            let query = const_string(args.get(query_idx)?, params)?;
            let weight_args = &args[query_idx + 1..];
            let weights = if weight_args.is_empty() {
                None
            } else {
                if weight_args.len() != fields.len() {
                    return None;
                }
                Some(
                    weight_args
                        .iter()
                        .map(|arg| const_f64(arg, params))
                        .collect::<Option<Vec<_>>>()?,
                )
            };
            return Some(OperatorTree::MultiFieldSearch {
                fields,
                queries: vec![query; query_idx],
                weights,
            });
        }
    }

    if args.len() < 4 || !args.len().is_multiple_of(2) {
        return None;
    }
    let n_fields = args.len() / 2;
    let mut fields = Vec::with_capacity(n_fields);
    let mut queries = Vec::with_capacity(n_fields);
    for i in 0..n_fields {
        fields.push(column_name(&args[2 * i])?);
        queries.push(const_string(&args[2 * i + 1], params)?);
    }
    Some(OperatorTree::MultiFieldSearch {
        fields,
        queries,
        weights: None,
    })
}

pub(super) fn lower_staged_retrieval(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    let mut stages = Vec::new();
    if matches!(args.first(), Some(ScalarExpr::Func { .. }))
        && named_arg_expr(args.first()?).is_none()
    {
        if args.is_empty() || !args.len().is_multiple_of(2) {
            return None;
        }
        for pair in args.chunks(2) {
            stages.push(MultiStageEntry {
                child: lower_signal_arg(&pair[0], params)?,
                cutoff: MultiStageCutoff::TopK(const_usize(&pair[1], params)?),
            });
        }
    } else {
        if args.is_empty() || !args.len().is_multiple_of(3) {
            return None;
        }
        for stage in args.chunks(3) {
            stages.push(MultiStageEntry {
                child: OperatorTree::Term {
                    query: const_string(&stage[1], params)?,
                    field: Some(column_name(&stage[0])?),
                    scoring: Some(TextScoringMode::BM25),
                    top_k: None,
                },
                cutoff: MultiStageCutoff::TopK(const_usize(&stage[2], params)?),
            });
        }
    }
    (!stages.is_empty()).then_some(OperatorTree::MultiStage { stages })
}

/// Compile a signal-function call into a node on the `[0, 1]` evidence scale:
/// exact fusion, robust pooling, attention, and learned combinations all need
/// a common numeric boundary even though they make different semantic claims.
///
/// - `bayesian_match` --> [`OperatorTree::Term`] with Bayesian BM25 scoring.
/// - `fts_match` text trees --> [`OperatorTree::BayesianScore`] around the
///   complete raw BM25 Boolean query.
/// - `knn_match` --> [`OperatorTree::CosineProbability`] wrapping a
///   [`OperatorTree::KNN`] child. At a fusion boundary the driver uses this
///   marker to fit prior-free evidence from the selected cosine query pool.
pub(super) fn lower_calibrated_signal(
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    match name {
        "bayesian_match" => try_lower_text_match(
            "bayesian_match",
            args,
            params,
            TextScoringMode::BayesianBM25,
        )
        .ok(),
        "fts_match" => try_lower_fts_match(args, params).ok(),
        "bayesian_match_with_prior" => lower_bayesian_match_with_prior(args, params),
        "knn_match" => try_lower_knn_match(args, params)
            .ok()
            .map(|tree| OperatorTree::CosineProbability(Box::new(tree))),
        "calibrated_vector_match" => lower_calibrated_vector_match(args, params),
        _ => None,
    }
}

/// Lower a function-call argument into a probability-domain signal node. Used
/// by exact evidence fusion, robust pooling, attention, and learned fusion so
/// the rewrite stays consistent across combination policies.
pub(super) fn lower_signal_arg(arg: &ScalarExpr, params: &[SQLParam]) -> Option<OperatorTree> {
    match arg {
        ScalarExpr::Func { name, args, .. } => {
            let lower = name.to_ascii_lowercase();
            lower_calibrated_signal(&lower, args, params)
        }
        _ => None,
    }
}

/// Lower any registered posting-list function used as an operator input.
/// Unlike fusion signals, sparse thresholding accepts raw BM25 scores, so
/// this path intentionally does not require probability calibration.
pub(super) fn lower_operator_arg(arg: &ScalarExpr, params: &[SQLParam]) -> Option<OperatorTree> {
    let ScalarExpr::Func { name, args, .. } = arg else {
        return None;
    };
    lower_function(name, args, params)
}

pub(super) fn field_name_arg(expr: &ScalarExpr, params: &[SQLParam]) -> Option<String> {
    column_name(expr).or_else(|| const_string(expr, params))
}

enum FtsDefaultField {
    Field(String),
    All,
}

impl FtsDefaultField {
    fn as_deref(&self) -> Option<&str> {
        match self {
            FtsDefaultField::Field(field) => Some(field),
            FtsDefaultField::All => None,
        }
    }
}

fn fts_default_field(expr: &ScalarExpr) -> Option<FtsDefaultField> {
    match expr {
        ScalarExpr::Column(name) => Some(FtsDefaultField::Field(name.clone())),
        ScalarExpr::QualifiedColumn { column, .. } => Some(FtsDefaultField::Field(column.clone())),
        ScalarExpr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => {
            Some(FtsDefaultField::All)
        }
        _ => None,
    }
}

pub(super) fn prepare_fts_probability_tree(tree: OperatorTree) -> OperatorTree {
    if is_text_query_tree(&tree) {
        let field = common_text_field(&tree);
        return OperatorTree::BayesianScore {
            source: Box::new(bind_fts_bm25_tree(tree)),
            field,
        };
    }

    match tree {
        OperatorTree::KNN {
            query_vector,
            k,
            field,
        } => OperatorTree::CosineProbability(Box::new(OperatorTree::KNN {
            query_vector,
            k,
            field,
        })),
        OperatorTree::Intersect(children) => OperatorTree::Intersect(
            children
                .into_iter()
                .map(prepare_fts_probability_tree)
                .collect(),
        ),
        OperatorTree::Union(children) => OperatorTree::Union(
            children
                .into_iter()
                .map(prepare_fts_probability_tree)
                .collect(),
        ),
        OperatorTree::Complement(child) => {
            OperatorTree::Complement(Box::new(prepare_fts_probability_tree(*child)))
        }
        OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
            OperatorTree::BayesianEvidenceFusion {
                signals: signals
                    .into_iter()
                    .map(prepare_fts_probability_tree)
                    .collect(),
                base_rate,
            }
        }
        OperatorTree::RobustPositiveEvidencePool {
            signals,
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        } => OperatorTree::RobustPositiveEvidencePool {
            signals: signals
                .into_iter()
                .map(prepare_fts_probability_tree)
                .collect(),
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        },
        OperatorTree::CosineProbability(child) => OperatorTree::CosineProbability(child),
        other => other,
    }
}

pub(super) fn is_text_query_tree(tree: &OperatorTree) -> bool {
    match tree {
        OperatorTree::Empty | OperatorTree::Term { .. } => true,
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().all(is_text_query_tree),
        OperatorTree::Complement(child) => is_text_query_tree(child),
        _ => false,
    }
}

pub(super) fn bind_fts_bm25_tree(tree: OperatorTree) -> OperatorTree {
    match tree {
        OperatorTree::Term {
            query,
            field,
            top_k,
            ..
        } => OperatorTree::Term {
            query,
            field,
            scoring: Some(TextScoringMode::BM25),
            top_k,
        },
        OperatorTree::Intersect(children) => {
            OperatorTree::Intersect(children.into_iter().map(bind_fts_bm25_tree).collect())
        }
        OperatorTree::Union(children) => {
            OperatorTree::Union(children.into_iter().map(bind_fts_bm25_tree).collect())
        }
        OperatorTree::Composed(children) => {
            OperatorTree::Composed(children.into_iter().map(bind_fts_bm25_tree).collect())
        }
        OperatorTree::Complement(child) => {
            OperatorTree::Complement(Box::new(bind_fts_bm25_tree(*child)))
        }
        other => other,
    }
}

pub(super) fn common_text_field(tree: &OperatorTree) -> Option<String> {
    fn collect_fields(tree: &OperatorTree, fields: &mut BTreeSet<Option<String>>) {
        match tree {
            OperatorTree::Term { field, .. } => {
                fields.insert(field.clone());
            }
            OperatorTree::Intersect(children)
            | OperatorTree::Union(children)
            | OperatorTree::Composed(children) => {
                for child in children {
                    collect_fields(child, fields);
                }
            }
            OperatorTree::Complement(child) => collect_fields(child, fields),
            _ => {}
        }
    }

    let mut fields = BTreeSet::new();
    collect_fields(tree, &mut fields);
    if fields.len() == 1 {
        fields.into_iter().next().flatten()
    } else {
        None
    }
}
