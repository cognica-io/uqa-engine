//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-emitting SQL function dispatch and retrieval helpers.

use std::collections::BTreeMap;

use uqa_core::{DocId, Value};
use uqa_sql::ast::{Expr, FromClause};
use uqa_sql::expr::{eval, value_to_vector, EvalContext};
use uqa_sql::registry::{lookup, FunctionKind};
use uqa_sql::{SQLError, SQLParam};

use crate::{Engine, ScoredEntry};

const SINGLE_FIELD_TEXT_MATCH_FUNCTIONS: [&str; 4] = [
    "text_match",
    "bayesian_match",
    "fts_match",
    "bayesian_match_with_prior",
];

/// Walk an expression tree and hand every text-match field argument to
/// `validate`. Used by the select runners to reject silently-empty
/// searches before the WHERE reaches either the operator-tree pipeline
/// or the legacy dispatch.
fn walk_text_match_fields(
    expr: &Expr,
    validate: &mut dyn FnMut(&Expr, &str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    match expr {
        Expr::Func {
            name, args, filter, ..
        } => {
            let lower = name.to_ascii_lowercase();
            if SINGLE_FIELD_TEXT_MATCH_FUNCTIONS.contains(&lower.as_str()) {
                if let Some(field_arg) = args.first() {
                    if !(lower == "fts_match" && fts_query_is_jsonpath(args.get(1))) {
                        validate(field_arg, &lower)?;
                    }
                }
            } else if lower == "multi_field_match" {
                match multi_field_match_shape(args)? {
                    MultiFieldMatchShape::FieldsThenQuery { fields, .. }
                    | MultiFieldMatchShape::Pairs { fields } => {
                        for field_arg in fields {
                            validate(field_arg, "multi_field_match")?;
                        }
                    }
                }
            }
            for arg in args {
                walk_text_match_fields(arg, validate)?;
            }
            if let Some(filter) = filter {
                walk_text_match_fields(filter, validate)?;
            }
            Ok(())
        }
        Expr::And(items) | Expr::Or(items) | Expr::Array(items) => {
            for item in items {
                walk_text_match_fields(item, validate)?;
            }
            Ok(())
        }
        Expr::Not(inner) => walk_text_match_fields(inner, validate),
        Expr::Binary { lhs, rhs, .. } => {
            walk_text_match_fields(lhs, validate)?;
            walk_text_match_fields(rhs, validate)
        }
        Expr::IsNull { expr, .. } => walk_text_match_fields(expr, validate),
        Expr::Between { expr, low, high } => {
            walk_text_match_fields(expr, validate)?;
            walk_text_match_fields(low, validate)?;
            walk_text_match_fields(high, validate)
        }
        Expr::InList { expr, list, .. } => {
            walk_text_match_fields(expr, validate)?;
            for item in list {
                walk_text_match_fields(item, validate)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn validate_expr_text_match_fields(
    engine: &Engine,
    table: &str,
    expr: &Expr,
) -> Result<(), SQLError> {
    walk_text_match_fields(
        expr,
        &mut |field_arg, function_name| match text_match_field_name(field_arg) {
            Some(TextMatchField::All) => {
                validate_text_match_all_fields(engine, table, function_name)
            }
            Some(TextMatchField::Named(field)) => {
                validate_text_match_field(engine, table, field, function_name)
            }
            None => Ok(()),
        },
    )
}

enum TextMatchField<'a> {
    All,
    Named(&'a str),
}

/// The `_all` pseudo-field arrives either as a string literal or as a
/// bare column reference, depending on how the query was written.
fn text_match_field_name(field_arg: &Expr) -> Option<TextMatchField<'_>> {
    match field_arg {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
            if name.is_empty() || name == "_all" {
                Some(TextMatchField::All)
            } else {
                Some(TextMatchField::Named(name))
            }
        }
        Expr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => Some(TextMatchField::All),
        _ => None,
    }
}

pub(super) fn validate_joined_expr_text_match_fields(
    engine: &Engine,
    from: &FromClause,
    expr: &Expr,
) -> Result<(), SQLError> {
    let mut tables: Vec<(Option<String>, String)> = Vec::new();
    let mut has_opaque_source = false;
    collect_from_tables(from, &mut tables, &mut has_opaque_source);
    walk_text_match_fields(expr, &mut |field_arg, function_name| {
        let (qualifier, column) = match field_arg {
            Expr::Column(name) => (None, name.as_str()),
            Expr::QualifiedColumn {
                qualifier, column, ..
            } => (Some(qualifier.as_str()), column.as_str()),
            _ => return Ok(()),
        };
        if column.is_empty() || column == "_all" {
            return Ok(());
        }
        if let Some(qualifier) = qualifier {
            let resolved = tables
                .iter()
                .find(|(alias, name)| alias.as_deref() == Some(qualifier) || name == qualifier);
            return match resolved {
                Some((_, table)) => validate_text_match_field(engine, table, column, function_name),
                // Unknown qualifiers can point at subqueries or CTEs the
                // validator cannot introspect.
                None => Ok(()),
            };
        }
        let containing: Vec<&String> = tables
            .iter()
            .map(|(_, name)| name)
            .filter(|name| engine.table_has_column(name, column))
            .collect();
        if containing.iter().any(|name| {
            engine
                .fts_fields_for_table(name)
                .iter()
                .any(|f| f == column)
        }) {
            return Ok(());
        }
        if let Some(table) = containing.first() {
            return validate_text_match_field(engine, table, column, function_name);
        }
        if has_opaque_source {
            return Ok(());
        }
        Err(SQLError::TypeMismatch(format!(
            "{function_name}: column `{column}` does not exist on any joined table"
        )))
    })
}

/// The `@@` operator doubles as a `JSONPath` match when the right-hand
/// side is a `$...` path literal; that form evaluates row-level JSON and
/// needs no text index.
fn fts_query_is_jsonpath(query_arg: Option<&Expr>) -> bool {
    matches!(
        query_arg,
        Some(Expr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')
    )
}

fn collect_from_tables(
    from: &FromClause,
    out: &mut Vec<(Option<String>, String)>,
    has_opaque_source: &mut bool,
) {
    match from {
        FromClause::Table { name, alias } => out.push((alias.clone(), name.clone())),
        FromClause::Join { left, right, .. } => {
            collect_from_tables(left, out, has_opaque_source);
            collect_from_tables(right, out, has_opaque_source);
        }
        _ => *has_opaque_source = true,
    }
}

pub(super) fn execute_function(
    engine: &Engine,
    table: &str,
    name: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_function_with_top_k(engine, table, name, args, params, None)
}

pub(super) fn execute_function_with_top_k(
    engine: &Engine,
    table: &str,
    name: &str,
    args: &[Expr],
    params: &[SQLParam],
    top_k: Option<usize>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let kind = lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.to_string()))?;
    match kind {
        FunctionKind::TextMatch => run_text_match(engine, table, args, params, top_k),
        FunctionKind::BayesianMatch => run_bayesian_match(engine, table, args, params, top_k),
        FunctionKind::FTSMatch => run_fts_match(engine, table, args, params),
        FunctionKind::BayesianMatchWithPrior => {
            run_bayesian_match_with_prior(engine, table, args, params)
        }
        FunctionKind::SparseThreshold => run_sparse_threshold(engine, table, args, params),
        FunctionKind::KNNMatch => run_knn_match(engine, table, args, params),
        FunctionKind::CalibratedVectorMatch => {
            run_calibrated_vector_match(engine, table, args, params)
        }
        FunctionKind::FuseLogOdds => run_fuse_log_odds(engine, table, args, params),
        FunctionKind::GraphPagerank => run_graph_pagerank(engine, args, params),
        FunctionKind::GraphHits => run_graph_hits(engine, args, params),
        FunctionKind::GraphBetweenness => run_graph_betweenness(engine, args, params),
        FunctionKind::GraphTraverse => run_graph_traverse(engine, args, params),
        FunctionKind::GraphNeighbors => run_graph_neighbors(engine, args, params),
        FunctionKind::MultiFieldMatch => run_multi_field_match(engine, table, args, params),
        FunctionKind::StagedRetrieval => run_staged_retrieval(engine, table, args, params),
        FunctionKind::DeepPredict => run_deep_predict(engine, args, params),
        FunctionKind::TraverseMatch => run_graph_traverse(engine, args, params),
        FunctionKind::TemporalTraverse => run_temporal_traverse(engine, args, params),
        FunctionKind::RPQ => run_rpq(engine, args, params),
        FunctionKind::GraphCreate => run_graph_create(engine, args, params),
        FunctionKind::GraphDrop => run_graph_drop(engine, args, params),
        FunctionKind::GraphEdges => run_graph_edges(engine, args, params),
        // The remaining UQA functions either return a non-posting
        // shape or are construction-time helpers; they reach the
        // projection-side handler instead of this row-emitting
        // dispatcher.
        FunctionKind::AttentionFusion | FunctionKind::LearnedFusion => {
            run_attention_fusion(engine, table, name, args, params)
        }
        FunctionKind::UQAHighlight
        | FunctionKind::UQAFacets
        | FunctionKind::ScoreBM25
        | FunctionKind::ScoreBayesianBM25
        | FunctionKind::DeepLearn
        | FunctionKind::Convolve
        | FunctionKind::Pool
        | FunctionKind::Flatten
        | FunctionKind::Dense
        | FunctionKind::Softmax
        | FunctionKind::Layer
        | FunctionKind::Model => Err(SQLError::Unsupported(format!(
            "row-emitting dispatch for `{name}` is handled elsewhere"
        ))),
    }
}

fn run_deep_predict(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "deep_predict".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = match eval(&args[0], &ctx)? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_predict.model must be a string, got {other:?}"
            )));
        }
    };
    let scores = engine
        .deep_predict(&name)
        .ok_or_else(|| SQLError::Unsupported(format!("unknown model {name:?}")))?;
    Ok(scores
        .into_iter()
        .map(|(doc_id, score)| ScoredEntry { doc_id, score })
        .collect())
}

fn run_staged_retrieval(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if matches!(args.first(), Some(Expr::Func { .. })) && !is_named_arg_expr(&args[0]) {
        if args.is_empty() || args.len() % 2 != 0 {
            return Err(SQLError::BadArity {
                name: "staged_retrieval".into(),
                expected: "pairs of (signal, top_k)".into(),
                actual: args.len(),
            });
        }
        let ctx = EvalContext::new(None, params).with_engine(engine);
        let mut current: Option<Vec<ScoredEntry>> = None;
        for pair in args.chunks(2) {
            let rows = run_scored_signal(engine, table, &pair[0], params, "staged_retrieval")?;
            let top_k = expect_usize(&pair[1], "staged_retrieval.top_k", &ctx)?;
            let mut scored = rows;
            if let Some(prior) = &current {
                let prior_ids: std::collections::BTreeSet<u64> =
                    prior.iter().map(|e| e.doc_id).collect();
                scored.retain(|e| prior_ids.contains(&e.doc_id));
            }
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(top_k);
            scored.sort_by_key(|e| e.doc_id);
            current = Some(scored);
        }
        return Ok(current.unwrap_or_default());
    }

    if args.is_empty() || args.len() % 3 != 0 {
        return Err(SQLError::BadArity {
            name: "staged_retrieval".into(),
            expected: ">= 3, multiple of 3 (field, query, top_k)".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let mode = crate::ScoringMode::BM25(uqa_scoring::BM25Params::default());
    let n_stages = args.len() / 3;
    let mut current: Option<Vec<ScoredEntry>> = None;
    for i in 0..n_stages {
        let field = expect_column_name(&args[3 * i], "staged_retrieval.field")?;
        let q = match eval(&args[3 * i + 1], &ctx)? {
            Value::Str(s) => s,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "staged_retrieval query must be string, got {other:?}"
                )));
            }
        };
        let top_k = expect_usize(&args[3 * i + 2], "staged_retrieval.top_k", &ctx)?;
        let mut scored = engine.search(table, &field, &q, &mode, usize::MAX);
        if let Some(prior) = &current {
            let prior_ids: std::collections::BTreeSet<u64> =
                prior.iter().map(|e| e.doc_id).collect();
            scored.retain(|e| prior_ids.contains(&e.doc_id));
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored.sort_by_key(|e| e.doc_id);
        current = Some(scored);
    }
    Ok(current.unwrap_or_default())
}

fn run_multi_field_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() < 3 {
        return Err(SQLError::BadArity {
            name: "multi_field_match".into(),
            expected: ">= 3 (fields..., query[, weights...])".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let (fields, queries, weights) = parse_multi_field_match_args(args, &ctx)?;
    for field in &fields {
        validate_text_match_field(engine, table, field, "multi_field_match")?;
    }
    let n_fields = fields.len();
    // Unmatched fields pad with the no-match prior floor rather than
    // 0.5: calibrated matched posteriors can sit below 0.5 on small
    // corpora, and a higher pad would rank documents that match more
    // fields below documents that match fewer.
    let no_match_pad = uqa_scoring::BayesianProbabilityTransform::no_match_prior();
    let mut per_doc: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    for (i, (field, q)) in fields.iter().zip(queries.iter()).enumerate() {
        let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
        let scored = engine.search(table, field, q, &mode, usize::MAX);
        for entry in scored {
            let slot = per_doc
                .entry(entry.doc_id)
                .or_insert_with(|| vec![no_match_pad; n_fields]);
            slot[i] = entry.score;
        }
    }
    // Pad missing slots so every doc has a full vector.
    for slot in per_doc.values_mut() {
        if slot.len() < n_fields {
            slot.resize(n_fields, no_match_pad);
        }
    }
    let mut out: Vec<ScoredEntry> = per_doc
        .into_iter()
        .map(|(doc_id, probs)| {
            let fused = if probs.len() == 1 {
                probs[0]
            } else {
                uqa_scoring::prob::log_odds_conjunction_weighted(&probs, &weights, 0.0)
                    .unwrap_or(0.5)
            };
            ScoredEntry {
                doc_id,
                score: fused,
            }
        })
        .collect();
    out.sort_by_key(|e| e.doc_id);
    Ok(out)
}

type MultiFieldMatchArgs = (Vec<String>, Vec<String>, Vec<f64>);

enum MultiFieldMatchShape<'a> {
    FieldsThenQuery {
        fields: Vec<&'a Expr>,
        query_idx: usize,
    },
    Pairs {
        fields: Vec<&'a Expr>,
    },
}

fn multi_field_match_shape(args: &[Expr]) -> Result<MultiFieldMatchShape<'_>, SQLError> {
    let first_non_column = args
        .iter()
        .position(|arg| !matches!(arg, Expr::Column(_) | Expr::QualifiedColumn { .. }));
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
    args: &[Expr],
    ctx: &EvalContext<'_>,
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
                )
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
    expr: &Expr,
    label: &str,
    ctx: &EvalContext<'_>,
) -> Result<String, SQLError> {
    match eval(expr, ctx)? {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be string, got {other:?}"
        ))),
    }
}

fn expect_f64_value(expr: &Expr, label: &str, ctx: &EvalContext<'_>) -> Result<f64, SQLError> {
    match eval(expr, ctx)? {
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

fn normalize_weights(weights: Vec<f64>) -> Vec<f64> {
    let total: f64 = weights.iter().sum();
    if total > 0.0 {
        weights.into_iter().map(|w| w / total).collect()
    } else {
        uniform_weights(weights.len())
    }
}

fn default_graph_name(engine: &Engine, function_name: &str) -> Result<String, SQLError> {
    let graphs = engine.list_graphs();
    match graphs.as_slice() {
        [name] => Ok(name.clone()),
        [] => Err(SQLError::Unsupported(format!(
            "{function_name} requires a graph argument because no graph is registered"
        ))),
        _ => Err(SQLError::Unsupported(format!(
            "{function_name} requires a graph argument because multiple graphs are registered: {}",
            graphs.join(", ")
        ))),
    }
}

fn expect_optional_graph_name(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
    function_name: &str,
) -> Result<String, SQLError> {
    match args {
        [] => default_graph_name(engine, function_name),
        [arg] => {
            let ctx = EvalContext::new(None, params).with_engine(engine);
            expect_string(arg, &format!("{function_name}.graph"), &ctx)
        }
        _ => Err(SQLError::BadArity {
            name: function_name.into(),
            expected: "0..=1".into(),
            actual: args.len(),
        }),
    }
}

pub(super) fn expect_optional_graph_value(
    engine: &Engine,
    value: Option<&Value>,
    function_name: &str,
) -> Result<String, SQLError> {
    match value {
        Some(Value::Str(name)) => Ok(name.clone()),
        Some(other) => Err(SQLError::TypeMismatch(format!(
            "{function_name}.graph must be string, got {other:?}"
        ))),
        None => default_graph_name(engine, function_name),
    }
}

pub(super) fn graph_pagerank_entries(
    engine: &Engine,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let entries = engine
        .graph_with(name, |store| {
            uqa_graph::PageRank::new(name)
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    Ok(entries)
}

pub(super) fn graph_hits_entries(
    engine: &Engine,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let entries = engine
        .graph_with(name, |store| {
            uqa_graph::HITS::new(name)
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    Ok(entries)
}

pub(super) fn graph_betweenness_entries(
    engine: &Engine,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let entries = engine
        .graph_with(name, |store| {
            uqa_graph::BetweennessCentrality::new(name)
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    Ok(entries)
}

fn run_graph_pagerank(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let name = expect_optional_graph_name(engine, args, params, "graph_pagerank")?;
    graph_pagerank_entries(engine, &name)
}

fn run_graph_hits(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let name = expect_optional_graph_name(engine, args, params, "graph_hits")?;
    graph_hits_entries(engine, &name)
}

fn run_graph_betweenness(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let name = expect_optional_graph_name(engine, args, params, "graph_betweenness")?;
    graph_betweenness_entries(engine, &name)
}

fn run_graph_traverse(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 4 {
        return Err(SQLError::BadArity {
            name: "graph_traverse".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_traverse.graph", &ctx)?;
    let start = expect_u64(&args[1], "graph_traverse.start", &ctx)?;
    let label = expect_optional_string(&args[2], "graph_traverse.label", &ctx)?;
    let max_hops = expect_u32(&args[3], "graph_traverse.max_hops", &ctx)?;
    let entries = engine
        .graph_with(&name, |store| {
            let mut traverse = uqa_graph::Traverse::new(start, &name).max_hops(max_hops);
            if let Some(l) = label.as_deref() {
                traverse = traverse.label(l);
            }
            traverse
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    Ok(entries)
}

fn run_graph_neighbors(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 4 {
        return Err(SQLError::BadArity {
            name: "graph_neighbors".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_neighbors.graph", &ctx)?;
    let vertex = expect_u64(&args[1], "graph_neighbors.vertex", &ctx)?;
    let label = expect_optional_string(&args[2], "graph_neighbors.label", &ctx)?;
    let direction_str = expect_string(&args[3], "graph_neighbors.direction", &ctx)?;
    let direction = match direction_str.to_ascii_lowercase().as_str() {
        "out" => uqa_graph::Direction::Out,
        "in" => uqa_graph::Direction::In,
        "both" => uqa_graph::Direction::Both,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "graph_neighbors.direction must be 'out'/'in'/'both', got {other:?}"
            )));
        }
    };
    let neighbors = engine
        .graph_with(&name, |store| {
            <uqa_graph::MemoryGraphStore as uqa_graph::GraphStore>::neighbors(
                store,
                vertex,
                label.as_deref(),
                direction,
                &name,
            )
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for nid in neighbors {
        if seen.insert(nid) {
            out.push(ScoredEntry {
                doc_id: nid,
                score: 1.0,
            });
        }
    }
    Ok(out)
}

pub(super) fn run_graph_create(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "graph_create".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_create.name", &ctx)?;
    engine.create_graph(name);
    Ok(Vec::new())
}

pub(super) fn run_graph_drop(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "graph_drop".into(),
            expected: "1 or 2".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_drop.name", &ctx)?;
    if let Some(cascade_expr) = args.get(1) {
        match eval(cascade_expr, &ctx)? {
            Value::Bool(true) => {}
            Value::Bool(false) if engine.has_graph(&name) => {
                return Err(SQLError::Unsupported(format!(
                    "cannot drop graph {name:?} without cascade"
                )));
            }
            Value::Bool(false) => {}
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "graph_drop.cascade must be a boolean, got {other:?}"
                )));
            }
        }
    }
    engine.drop_graph(&name);
    Ok(Vec::new())
}

/// `graph_edges(graph [, label])` -- emit one entry per edge in the
/// named graph. The `doc_id` carries the edge id; the score is the
/// raw edge weight (`1.0` when no `weight` property is present).
fn run_graph_edges(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.is_empty() || args.len() > 2 {
        return Err(SQLError::BadArity {
            name: "graph_edges".into(),
            expected: "1..=2".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_edges.graph", &ctx)?;
    let label = if args.len() == 2 {
        expect_optional_string(&args[1], "graph_edges.label", &ctx)?
    } else {
        None
    };
    let edges = engine
        .graph_with(&name, |store| {
            <uqa_graph::MemoryGraphStore as uqa_graph::GraphStore>::edges_in_graph(store, &name)
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    let mut out = Vec::new();
    for edge in edges {
        if let Some(target_label) = label.as_deref() {
            if edge.label != target_label {
                continue;
            }
        }
        let weight = match edge.properties.get("weight") {
            Some(Value::Float(f)) => *f,
            Some(Value::Int(i)) => *i as f64,
            Some(Value::Decimal(d)) => d.to_f64().ok_or_else(|| {
                SQLError::TypeMismatch("graph_edges.weight decimal is outside f64 range".into())
            })?,
            _ => 1.0,
        };
        out.push(ScoredEntry {
            doc_id: edge.edge_id,
            score: weight,
        });
    }
    Ok(out)
}

/// `temporal_traverse(graph, start, label, max_hops, t_min, t_max)`
/// -- BFS traversal that respects edge `valid_from` / `valid_to`
/// properties. Emits `(vertex_id, score)` weighted by hop distance,
/// matching the canonical UQA behavior's shape.
fn run_temporal_traverse(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 6 {
        return Err(SQLError::BadArity {
            name: "temporal_traverse".into(),
            expected: "6".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "temporal_traverse.graph", &ctx)?;
    let start = expect_u64(&args[1], "temporal_traverse.start", &ctx)?;
    let label = expect_optional_string(&args[2], "temporal_traverse.label", &ctx)?;
    let max_hops = expect_usize(&args[3], "temporal_traverse.max_hops", &ctx)?;
    let t_min = match eval(&args[4], &ctx)? {
        Value::Int(n) => n as f64,
        Value::Float(f) => f,
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch("temporal_traverse.t_min decimal is outside f64 range".into())
        })?,
        Value::Null => f64::NEG_INFINITY,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "temporal_traverse.t_min must be numeric, got {other:?}"
            )));
        }
    };
    let t_max = match eval(&args[5], &ctx)? {
        Value::Int(n) => n as f64,
        Value::Float(f) => f,
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch("temporal_traverse.t_max decimal is outside f64 range".into())
        })?,
        Value::Null => f64::INFINITY,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "temporal_traverse.t_max must be numeric, got {other:?}"
            )));
        }
    };
    let traversed = engine
        .graph_with(
            &name,
            |store| -> Result<std::collections::BTreeMap<u64, f64>, SQLError> {
                use std::collections::VecDeque;
                use uqa_graph::GraphStore;
                let mut visited: std::collections::BTreeMap<u64, f64> =
                    std::collections::BTreeMap::new();
                let mut queue: VecDeque<(u64, usize)> = VecDeque::new();
                queue.push_back((start, 0));
                visited.insert(start, 1.0);
                while let Some((v, depth)) = queue.pop_front() {
                    if depth >= max_hops {
                        continue;
                    }
                    let edges = store.out_edge_ids(v, &name);
                    for eid in edges {
                        let Some(edge) = store.get_edge(eid) else {
                            continue;
                        };
                        if let Some(target_label) = label.as_deref() {
                            if edge.label != target_label {
                                continue;
                            }
                        }
                        // Read the edge's temporal range; fall back to
                        // unbounded when the property is missing.
                        let edge_from = match edge.properties.get("valid_from") {
                            Some(Value::Int(n)) => *n as f64,
                            Some(Value::Float(f)) => *f,
                            Some(Value::Decimal(d)) => d.to_f64().ok_or_else(|| {
                                SQLError::TypeMismatch(
                                    "temporal_traverse.valid_from decimal is outside f64 range"
                                        .into(),
                                )
                            })?,
                            _ => f64::NEG_INFINITY,
                        };
                        let edge_to = match edge.properties.get("valid_to") {
                            Some(Value::Int(n)) => *n as f64,
                            Some(Value::Float(f)) => *f,
                            Some(Value::Decimal(d)) => d.to_f64().ok_or_else(|| {
                                SQLError::TypeMismatch(
                                    "temporal_traverse.valid_to decimal is outside f64 range"
                                        .into(),
                                )
                            })?,
                            _ => f64::INFINITY,
                        };
                        if edge_to < t_min || edge_from > t_max {
                            continue;
                        }
                        let nbr = edge.target_id;
                        let score = 1.0 / ((depth + 1) as f64 + 1.0);
                        if let std::collections::btree_map::Entry::Vacant(slot) = visited.entry(nbr)
                        {
                            slot.insert(score);
                            queue.push_back((nbr, depth + 1));
                        }
                    }
                }
                Ok(visited)
            },
        )
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))??;
    let mut out: Vec<ScoredEntry> = traversed
        .into_iter()
        .map(|(v, score)| ScoredEntry { doc_id: v, score })
        .collect();
    out.sort_by_key(|e| e.doc_id);
    Ok(out)
}

/// `rpq(expr, start [, graph])` - evaluate a Regular Path Query
/// (Definition 5.1.2). Mirrors the canonical UQA implementation's
/// `Engine.sql("SELECT * FROM rpq(expr, start [, graph])")`.
fn run_rpq(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if !(2..=3).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "rpq".into(),
            expected: "2..=3".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let expr_str = expect_string(&args[0], "rpq.expr", &ctx)?;
    let start = expect_u64(&args[1], "rpq.start", &ctx)?;
    let graph = if args.len() == 3 {
        expect_string(&args[2], "rpq.graph", &ctx)?
    } else {
        default_graph_name(engine, "rpq")?
    };
    let path =
        uqa_graph::parse_rpq(&expr_str).map_err(|e| SQLError::Unsupported(format!("{e:?}")))?;
    let entries = engine
        .graph_with(&graph, |store| {
            uqa_graph::RegularPathQuery::new(path, &graph)
                .from_vertex(start)
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {graph:?}")))?;
    Ok(entries)
}

fn expect_string(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<String, SQLError> {
    match eval(expr, ctx)? {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a string, got {other:?}"
        ))),
    }
}

fn expect_optional_string(
    expr: &Expr,
    name: &str,
    ctx: &EvalContext,
) -> Result<Option<String>, SQLError> {
    match eval(expr, ctx)? {
        Value::Null => Ok(None),
        Value::Str(s) if s.is_empty() => Ok(None),
        Value::Str(s) => Ok(Some(s)),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a string or NULL, got {other:?}"
        ))),
    }
}

fn expect_u64(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<u64, SQLError> {
    match eval(expr, ctx)? {
        Value::Int(n) if n >= 0 => Ok(n as u64),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a non-negative integer, got {other:?}"
        ))),
    }
}

pub(crate) fn run_text_match_public(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_text_match(engine, table, args, params, None)
}

pub(crate) fn run_bayesian_match_public(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_bayesian_match(engine, table, args, params, None)
}

pub(crate) fn run_knn_match_public(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_knn_match(engine, table, args, params)
}

pub(crate) fn run_bayesian_match_with_prior_public(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_bayesian_match_with_prior(engine, table, args, params)
}

pub(crate) fn run_calibrated_vector_match_public(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_calibrated_vector_match(engine, table, args, params)
}

pub(crate) fn run_multi_field_match_public(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_multi_field_match(engine, table, args, params)
}

fn expect_u32(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<u32, SQLError> {
    let max_u32_as_i64: i64 = i64::from(u32::MAX);
    match eval(expr, ctx)? {
        Value::Int(n) if (0..=max_u32_as_i64).contains(&n) => Ok(n as u32),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must fit in u32, got {other:?}"
        ))),
    }
}

fn run_text_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
    top_k: Option<usize>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_text_match_scored(
        engine,
        table,
        args,
        params,
        "text_match",
        crate::ScoringMode::BM25(uqa_scoring::BM25Params::default()),
        top_k,
    )
}

fn run_bayesian_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
    top_k: Option<usize>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_text_match_scored(
        engine,
        table,
        args,
        params,
        "bayesian_match",
        crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default()),
        top_k,
    )
}

/// Reject silently-empty text searches up front: a match function whose
/// field is not a real column, or is a column without a text index,
/// previously returned zero rows with no diagnostic.
fn validate_text_match_field(
    engine: &Engine,
    table: &str,
    field: &str,
    function_name: &str,
) -> Result<(), SQLError> {
    if engine.table_columns(table).is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: unknown table `{table}`"
        )));
    }
    if !engine.table_has_column(table, field) {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: column `{field}` does not exist on table `{table}`"
        )));
    }
    if !engine
        .fts_fields_for_table(table)
        .iter()
        .any(|fts| fts == field)
    {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: column `{table}.{field}` has no text index; \
             create one with CREATE INDEX ... ON {table} USING gin ({field})"
        )));
    }
    Ok(())
}

fn validate_text_match_all_fields(
    engine: &Engine,
    table: &str,
    function_name: &str,
) -> Result<(), SQLError> {
    if engine.table_columns(table).is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: unknown table `{table}`"
        )));
    }
    if engine.fts_fields_for_table(table).is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: table `{table}` has no text-indexed columns; \
             create one with CREATE INDEX ... ON {table} USING gin (...)"
        )));
    }
    Ok(())
}

fn run_text_match_scored(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
    function_name: &str,
    mode: crate::ScoringMode,
    top_k: Option<usize>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: function_name.into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let field = match &args[0] {
        Expr::Column(name) => name.clone(),
        Expr::QualifiedColumn { column, .. } => column.clone(),
        Expr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => "_all".to_string(),
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
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let query_value = eval(&args[1], &ctx)?;
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
        for field_name in engine.fts_fields_for_table(table) {
            for entry in engine.search(table, &field_name, &query, &mode, usize::MAX) {
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
    Ok(engine.search(table, &field, &query, &mode, top_k.unwrap_or(usize::MAX)))
}

fn run_fts_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "fts_match".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let default_field = match &args[0] {
        Expr::Column(name) => Some(name.clone()),
        Expr::QualifiedColumn { column, .. } => Some(column.clone()),
        Expr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => None,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "fts_match.field must be a column reference, got {other:?}"
            )));
        }
    };
    if !fts_query_is_jsonpath(args.get(1)) {
        match default_field.as_deref() {
            Some(field) if !field.is_empty() && field != "_all" => {
                validate_text_match_field(engine, table, field, "fts_match")?;
            }
            _ => validate_text_match_all_fields(engine, table, "fts_match")?,
        }
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let query = expect_string(&args[1], "fts_match.query", &ctx)?;
    let tokenizer = |_field: Option<&str>, phrase: &str| {
        phrase
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    };
    uqa_sql::compile_fts_query_string(&query, default_field.as_deref(), &tokenizer)?;
    let expr = Expr::Func {
        name: "fts_match".into(),
        args: args.to_vec(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    Ok(
        crate::operator_tree_bridge::run_optimised(engine, table, Some(&expr), params)?
            .unwrap_or_default(),
    )
}

fn run_bayesian_match_with_prior(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
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
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let query = expect_string(&args[1], "bayesian_match_with_prior.query", &ctx)?;
    let mode = expect_string(&args[3], "bayesian_match_with_prior.mode", &ctx)?;

    let base = run_bayesian_match(
        engine,
        table,
        &[Expr::Column(field), Expr::Literal(Value::Str(query))],
        params,
        None,
    )?;
    let prior_fn = prior_fn_for_mode(&mode, &prior_field)?;
    Ok(base
        .into_iter()
        .map(|entry| {
            let document = engine.get_document(table, entry.doc_id).unwrap_or_default();
            let prior = prior_fn(&document).clamp(1e-10, 1.0 - 1e-10);
            ScoredEntry {
                doc_id: entry.doc_id,
                score: combine_probability_with_prior(entry.score, prior),
            }
        })
        .collect())
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

fn named_arg_expr(expr: &Expr) -> Option<(&str, &Expr)> {
    let Expr::Func { name, args, .. } = expr else {
        return None;
    };
    if name != "__named_arg" || args.len() != 2 {
        return None;
    }
    let Expr::Literal(Value::Str(arg_name)) = &args[0] else {
        return None;
    };
    Some((arg_name.as_str(), &args[1]))
}

fn is_named_arg_expr(expr: &Expr) -> bool {
    named_arg_expr(expr).is_some()
}

fn run_scored_signal(
    engine: &Engine,
    table: &str,
    expr: &Expr,
    params: &[SQLParam],
    parent: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let Expr::Func {
        name, args: inner, ..
    } = expr
    else {
        return Err(SQLError::Unsupported(format!(
            "{parent} signal must be a function call"
        )));
    };
    match lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))? {
        FunctionKind::TextMatch => run_text_match(engine, table, inner, params, None),
        FunctionKind::BayesianMatch => run_bayesian_match(engine, table, inner, params, None),
        FunctionKind::FTSMatch => run_fts_match(engine, table, inner, params),
        FunctionKind::BayesianMatchWithPrior => {
            run_bayesian_match_with_prior(engine, table, inner, params)
        }
        FunctionKind::KNNMatch => run_knn_match(engine, table, inner, params),
        FunctionKind::CalibratedVectorMatch => {
            run_calibrated_vector_match(engine, table, inner, params)
        }
        _ => Err(SQLError::Unsupported(format!(
            "function {name} cannot be nested under {parent}"
        ))),
    }
}

fn run_attention_fusion(
    engine: &Engine,
    table: &str,
    function_name: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() < 2 {
        return Err(SQLError::BadArity {
            name: "fuse_attention".into(),
            expected: ">=2".into(),
            actual: args.len(),
        });
    }

    let mut score_maps: Vec<std::collections::BTreeMap<DocId, f64>> =
        Vec::with_capacity(args.len());
    let mut all_doc_ids = std::collections::BTreeSet::new();
    for arg in args {
        if is_named_arg_expr(arg) {
            continue;
        }
        let Expr::Func {
            name, args: inner, ..
        } = arg
        else {
            return Err(SQLError::Unsupported(
                "fuse_attention arguments must be function calls".into(),
            ));
        };
        let rows = match lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))? {
            FunctionKind::TextMatch => {
                return Err(non_probability_signal_error(name, function_name));
            }
            FunctionKind::BayesianMatch => run_bayesian_match(engine, table, inner, params, None)?,
            FunctionKind::FTSMatch => run_fts_match(engine, table, inner, params)?,
            FunctionKind::BayesianMatchWithPrior => {
                run_bayesian_match_with_prior(engine, table, inner, params)?
            }
            FunctionKind::KNNMatch => {
                cosine_rows_to_probabilities(run_knn_match(engine, table, inner, params)?)
            }
            FunctionKind::CalibratedVectorMatch => {
                run_calibrated_vector_match(engine, table, inner, params)?
            }
            _ => {
                return Err(SQLError::Unsupported(format!(
                    "function {name} cannot be nested under fuse_attention"
                )));
            }
        };
        let mut map = std::collections::BTreeMap::new();
        for row in rows {
            all_doc_ids.insert(row.doc_id);
            map.insert(row.doc_id, row.score.clamp(1e-10, 1.0 - 1e-10));
        }
        score_maps.push(map);
    }

    if score_maps.is_empty() {
        return Err(SQLError::BadArity {
            name: "fuse_attention".into(),
            expected: ">=1 signal".into(),
            actual: 0,
        });
    }

    let n = score_maps.len() as f64;
    Ok(all_doc_ids
        .into_iter()
        .map(|doc_id| {
            let score = score_maps
                .iter()
                .map(|map| map.get(&doc_id).copied().unwrap_or(0.5))
                .sum::<f64>()
                / n;
            ScoredEntry { doc_id, score }
        })
        .collect())
}

fn non_probability_signal_error(name: &str, parent: &str) -> SQLError {
    SQLError::TypeMismatch(format!(
        "{parent} requires probability-valued signals; `{name}` returns BM25 scores, use `bayesian_match`"
    ))
}

fn cosine_rows_to_probabilities(mut rows: Vec<ScoredEntry>) -> Vec<ScoredEntry> {
    for row in &mut rows {
        row.score = uqa_scoring::cosine_to_probability(row.score);
    }
    rows
}

fn run_sparse_threshold(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "sparse_threshold".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let Expr::Func {
        name, args: inner, ..
    } = &args[0]
    else {
        return Err(SQLError::Unsupported(
            "sparse_threshold source must be a function call".into(),
        ));
    };
    let rows = match lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))? {
        FunctionKind::TextMatch => run_text_match(engine, table, inner, params, None)?,
        FunctionKind::BayesianMatch => run_bayesian_match(engine, table, inner, params, None)?,
        FunctionKind::BayesianMatchWithPrior => {
            run_bayesian_match_with_prior(engine, table, inner, params)?
        }
        FunctionKind::KNNMatch => run_knn_match(engine, table, inner, params)?,
        FunctionKind::CalibratedVectorMatch => {
            run_calibrated_vector_match(engine, table, inner, params)?
        }
        _ => {
            return Err(SQLError::Unsupported(format!(
                "function {name} cannot be nested under sparse_threshold"
            )));
        }
    };
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let threshold = expect_f64_value(&args[1], "sparse_threshold.threshold", &ctx)?;
    Ok(rows
        .into_iter()
        .filter_map(|entry| {
            let adjusted = entry.score - threshold;
            (adjusted > 0.0).then_some(ScoredEntry {
                doc_id: entry.doc_id,
                score: adjusted,
            })
        })
        .collect())
}

fn run_knn_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 3 {
        return Err(SQLError::BadArity {
            name: "knn_match".into(),
            expected: "3".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "knn_match.field")?;
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let vec_value = eval(&args[1], &ctx)?;
    let query_vector = value_to_vector(&vec_value)?;
    let k = expect_usize(&args[2], "knn_match.k", &ctx)?;
    Ok(engine.knn_search(table, &field, query_vector, k))
}

fn run_calibrated_vector_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "calibrated_vector_match".into(),
            expected: "3..=4".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let field = expect_field_name_or_string(&args[0], "calibrated_vector_match.field", &ctx)?;
    let query_vector = value_to_vector(&eval(&args[1], &ctx)?)?;
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
    let Some((ctx, _)) = engine.snapshot_context(table) else {
        return Ok(Vec::new());
    };
    use uqa_operators::base::Operator;
    let op = uqa_operators::CalibratedVectorOperator::new(query_vector, k, field);
    let pl = op.execute(&ctx);
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
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    });
    Ok(out)
}

fn run_fuse_log_odds(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() < 2 {
        return Err(SQLError::BadArity {
            name: "fuse_log_odds".into(),
            expected: ">=2".into(),
            actual: args.len(),
        });
    }
    let mut alpha = 0.5;
    let mut score_maps: Vec<std::collections::BTreeMap<DocId, f64>> =
        Vec::with_capacity(args.len());
    let mut all_doc_ids = std::collections::BTreeSet::new();
    let ctx = EvalContext::new(None, params).with_engine(engine);
    for arg in args {
        if let Some((name, value_expr)) = named_arg_expr(arg) {
            if name.eq_ignore_ascii_case("alpha") {
                alpha = expect_f64_value(value_expr, "fuse_log_odds.alpha", &ctx)?;
            }
            continue;
        }
        match arg {
            Expr::Func {
                name, args: inner, ..
            } => {
                let kind = lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))?;
                let rows = match kind {
                    FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
                        if inner.len() != 2 {
                            return Err(SQLError::BadArity {
                                name: name.clone(),
                                expected: "2".into(),
                                actual: inner.len(),
                            });
                        }
                        match kind {
                            FunctionKind::TextMatch => {
                                return Err(non_probability_signal_error(name, "fuse_log_odds"));
                            }
                            FunctionKind::BayesianMatch => {
                                run_bayesian_match(engine, table, inner, params, None)?
                            }
                            _ => unreachable!("matched text scoring function"),
                        }
                    }
                    FunctionKind::FTSMatch => run_fts_match(engine, table, inner, params)?,
                    FunctionKind::BayesianMatchWithPrior => {
                        run_bayesian_match_with_prior(engine, table, inner, params)?
                    }
                    FunctionKind::KNNMatch => {
                        if inner.len() != 3 {
                            return Err(SQLError::BadArity {
                                name: name.clone(),
                                expected: "3".into(),
                                actual: inner.len(),
                            });
                        }
                        cosine_rows_to_probabilities(run_knn_match(engine, table, inner, params)?)
                    }
                    FunctionKind::CalibratedVectorMatch => {
                        run_calibrated_vector_match(engine, table, inner, params)?
                    }
                    FunctionKind::FuseLogOdds => {
                        return Err(SQLError::Unsupported(
                            "nested fuse_log_odds is not supported".into(),
                        ));
                    }
                    FunctionKind::GraphPagerank
                    | FunctionKind::GraphHits
                    | FunctionKind::GraphBetweenness
                    | FunctionKind::GraphTraverse
                    | FunctionKind::GraphNeighbors
                    | FunctionKind::MultiFieldMatch
                    | FunctionKind::StagedRetrieval
                    | FunctionKind::DeepPredict
                    | FunctionKind::UQAHighlight
                    | FunctionKind::UQAFacets
                    | FunctionKind::TraverseMatch
                    | FunctionKind::TemporalTraverse
                    | FunctionKind::RPQ
                    | FunctionKind::GraphCreate
                    | FunctionKind::GraphDrop
                    | FunctionKind::GraphEdges
                    | FunctionKind::AttentionFusion
                    | FunctionKind::LearnedFusion
                    | FunctionKind::ScoreBM25
                    | FunctionKind::ScoreBayesianBM25
                    | FunctionKind::SparseThreshold
                    | FunctionKind::DeepLearn
                    | FunctionKind::Convolve
                    | FunctionKind::Pool
                    | FunctionKind::Flatten
                    | FunctionKind::Dense
                    | FunctionKind::Softmax
                    | FunctionKind::Layer
                    | FunctionKind::Model => {
                        return Err(SQLError::Unsupported(format!(
                            "function {name} cannot be nested under fuse_log_odds"
                        )));
                    }
                };
                let mut map = std::collections::BTreeMap::new();
                for row in rows {
                    all_doc_ids.insert(row.doc_id);
                    map.insert(row.doc_id, row.score.clamp(1e-10, 1.0 - 1e-10));
                }
                score_maps.push(map);
            }
            Expr::Literal(Value::Float(v)) => {
                alpha = *v;
            }
            Expr::Literal(Value::Int(v)) => {
                alpha = *v as f64;
            }
            Expr::Literal(Value::Decimal(v)) => {
                alpha = v.to_f64().ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "fuse_log_odds.alpha decimal is outside f64 range".into(),
                    )
                })?;
            }
            Expr::Literal(Value::Str(_)) => {
                // Compatibility with the canonical UQA implementation's optional gating string
                // argument. Gating is a fusion-layer concern; the SQL
                // engine keeps the same calibrated score semantics.
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "fuse_log_odds argument must be a function call, got {other:?}"
                )));
            }
        }
    }
    if score_maps.len() < 2 {
        return Err(SQLError::BadArity {
            name: "fuse_log_odds".into(),
            expected: ">=2 signal functions".into(),
            actual: score_maps.len(),
        });
    }
    let n = score_maps.len();
    Ok(all_doc_ids
        .into_iter()
        .map(|doc_id| {
            let probs: Vec<f64> = score_maps
                .iter()
                .map(|map| map.get(&doc_id).copied().unwrap_or(0.5))
                .collect();
            let score = if n == 1 {
                probs[0]
            } else {
                uqa_scoring::log_odds_conjunction(&probs, alpha)
            };
            ScoredEntry { doc_id, score }
        })
        .collect())
}

pub(super) fn expect_column_name(expr: &Expr, label: &str) -> Result<String, SQLError> {
    match expr {
        Expr::Column(name) => Ok(name.clone()),
        Expr::QualifiedColumn { column, .. } => Ok(column.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be a column reference, got {other:?}"
        ))),
    }
}

fn expect_field_name_or_string(
    expr: &Expr,
    label: &str,
    ctx: &EvalContext<'_>,
) -> Result<String, SQLError> {
    match expr {
        Expr::Column(name) => Ok(name.clone()),
        Expr::QualifiedColumn { column, .. } => Ok(column.clone()),
        _ => expect_string(expr, label, ctx),
    }
}

fn expect_usize(expr: &Expr, label: &str, ctx: &EvalContext<'_>) -> Result<usize, SQLError> {
    let v = eval(expr, ctx)?;
    match v {
        Value::Int(n) if n >= 0 => Ok(n as usize),
        Value::Int(_) => Err(SQLError::TypeMismatch(format!("{label} must be >= 0"))),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be an integer, got {other:?}"
        ))),
    }
}
