//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-emitting SQL function dispatch and retrieval helpers.

use std::collections::BTreeMap;

use uqa_core::{DocId, Value};
use uqa_execution::{eval_scalar, ScalarEvalContext, ScalarExpr};
use uqa_operators::OperatorTree;
use uqa_planner::SourcePlan;
use uqa_sql::expr::value_to_vector;
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
/// searches before the WHERE reaches either the operator-tree access path
/// or scalar evaluation in the relational filter node.
fn walk_text_match_fields(
    expr: &ScalarExpr,
    validate: &mut dyn FnMut(&ScalarExpr, &str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    match expr {
        ScalarExpr::Func {
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
        ScalarExpr::And(items) | ScalarExpr::Or(items) | ScalarExpr::Array(items) => {
            for item in items {
                walk_text_match_fields(item, validate)?;
            }
            Ok(())
        }
        ScalarExpr::Not(inner) => walk_text_match_fields(inner, validate),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            walk_text_match_fields(lhs, validate)?;
            walk_text_match_fields(rhs, validate)
        }
        ScalarExpr::IsNull { expr, .. } => walk_text_match_fields(expr, validate),
        ScalarExpr::Between { expr, low, high } => {
            walk_text_match_fields(expr, validate)?;
            walk_text_match_fields(low, validate)?;
            walk_text_match_fields(high, validate)
        }
        ScalarExpr::InList { expr, list, .. } => {
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
    expr: &ScalarExpr,
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
fn text_match_field_name(field_arg: &ScalarExpr) -> Option<TextMatchField<'_>> {
    match field_arg {
        ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. } => {
            if name.is_empty() || name == "_all" {
                Some(TextMatchField::All)
            } else {
                Some(TextMatchField::Named(name))
            }
        }
        ScalarExpr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => {
            Some(TextMatchField::All)
        }
        _ => None,
    }
}

pub(super) fn validate_joined_expr_text_match_fields(
    engine: &Engine,
    from: &SourcePlan,
    expr: &ScalarExpr,
) -> Result<(), SQLError> {
    let mut tables: Vec<(Option<String>, String)> = Vec::new();
    let mut has_opaque_source = false;
    collect_from_tables(from, &mut tables, &mut has_opaque_source);
    walk_text_match_fields(expr, &mut |field_arg, function_name| {
        let (qualifier, column) = match field_arg {
            ScalarExpr::Column(name) => (None, name.as_str()),
            ScalarExpr::QualifiedColumn {
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
        let mut containing: Vec<&String> = Vec::new();
        for (_, name) in &tables {
            if engine
                .table_has_column(name, column)
                .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?
            {
                containing.push(name);
            }
        }
        for name in &containing {
            if engine
                .fts_fields_for_table(name)?
                .iter()
                .any(|f| f == column)
            {
                return Ok(());
            }
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
fn fts_query_is_jsonpath(query_arg: Option<&ScalarExpr>) -> bool {
    matches!(
        query_arg,
        Some(ScalarExpr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')
    )
}

fn collect_from_tables(
    from: &SourcePlan,
    out: &mut Vec<(Option<String>, String)>,
    has_opaque_source: &mut bool,
) {
    match from {
        SourcePlan::Table { name, alias } => out.push((alias.clone(), name.clone())),
        SourcePlan::Join { left, right, .. } => {
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
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_function_with_top_k(engine, table, name, args, params, None)
}

pub(super) fn execute_function_with_top_k(
    engine: &Engine,
    table: &str,
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    top_k: Option<usize>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let kind = lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.to_string()))?;
    match kind {
        FunctionKind::GraphCreate => run_graph_create(engine, args, params),
        FunctionKind::GraphDrop => run_graph_drop(engine, args, params),
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
        FunctionKind::TextMatch
        | FunctionKind::BayesianMatch
        | FunctionKind::FTSMatch
        | FunctionKind::BayesianMatchWithPrior
        | FunctionKind::SparseThreshold
        | FunctionKind::KNNMatch
        | FunctionKind::CalibratedVectorMatch
        | FunctionKind::FuseLogOdds
        | FunctionKind::PositiveEvidencePool
        | FunctionKind::BayesianEvidenceFusion
        | FunctionKind::GraphPagerank
        | FunctionKind::GraphHits
        | FunctionKind::GraphBetweenness
        | FunctionKind::GraphTraverse
        | FunctionKind::GraphNeighbors
        | FunctionKind::MultiFieldMatch
        | FunctionKind::StagedRetrieval
        | FunctionKind::DeepPredict
        | FunctionKind::TraverseMatch
        | FunctionKind::TemporalTraverse
        | FunctionKind::RPQ
        | FunctionKind::GraphEdges
        | FunctionKind::AttentionFusion
        | FunctionKind::LearnedFusion => {
            let tree =
                crate::operator_tree_bridge::lower_sql_function_bound(engine, name, args, params)?;
            let tree = match top_k {
                Some(k) => plan_bound_text_top_k(engine, table, tree, k)?,
                None => tree,
            };
            let posting = crate::operator_tree_bridge::expect_posting_output(
                crate::operator_tree_bridge::execute_operator_tree_in_execution(
                    engine, table, params, &tree,
                )?,
                name,
            )?;
            let posting = match top_k {
                Some(k) => posting.ranked().select_top_k(k),
                None => posting,
            };
            Ok(posting
                .entries()
                .iter()
                .map(|entry| ScoredEntry {
                    doc_id: entry.doc_id,
                    score: entry.payload.score,
                })
                .collect())
        }
    }
}

fn plan_bound_text_top_k(
    engine: &Engine,
    table: &str,
    tree: uqa_operators::OperatorTree,
    top_k: usize,
) -> Result<uqa_operators::OperatorTree, SQLError> {
    let (query, field, scoring) = match tree {
        uqa_operators::OperatorTree::Term {
            query,
            field: Some(field),
            scoring: Some(scoring),
            top_k: None,
        } => (query, field, scoring),
        other => return Ok(other),
    };
    let mode = match scoring {
        uqa_operators::TextScoringMode::BM25 => {
            crate::ScoringMode::BM25(crate::BM25Params::default())
        }
        uqa_operators::TextScoringMode::BayesianBM25 => crate::ScoringMode::BayesianBM25(
            engine.bayesian_params_for_in_execution(table, &field)?,
        ),
        uqa_operators::TextScoringMode::CustomBM25(params) => crate::ScoringMode::BM25(params),
        uqa_operators::TextScoringMode::CustomBayesianBM25(params) => {
            crate::ScoringMode::BayesianBM25(params)
        }
    };
    engine.plan_text_top_k_tree(table, &field, &query, &mode, scoring, top_k)
}

#[derive(Clone, Copy)]
enum RetrievalExecution {
    Public,
    InExecution,
}

impl RetrievalExecution {
    fn bayesian_params(
        self,
        engine: &Engine,
        table: &str,
        field: &str,
    ) -> Result<uqa_scoring::BayesianBM25Params, SQLError> {
        match self {
            Self::Public => engine.bayesian_params_for(table, field),
            Self::InExecution => engine.bayesian_params_for_in_execution(table, field),
        }
    }

    fn search(
        self,
        engine: &Engine,
        table: &str,
        field: &str,
        query: &str,
        mode: &crate::ScoringMode,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        match self {
            Self::Public => engine.search(table, field, query, mode, top_k),
            Self::InExecution => engine.search_leaf(table, field, query, mode, top_k, None),
        }
    }
}

fn run_multi_field_match(
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

enum MultiFieldMatchShape<'a> {
    FieldsThenQuery {
        fields: Vec<&'a ScalarExpr>,
        query_idx: usize,
    },
    Pairs {
        fields: Vec<&'a ScalarExpr>,
    },
}

fn multi_field_match_shape(args: &[ScalarExpr]) -> Result<MultiFieldMatchShape<'_>, SQLError> {
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

fn expect_f64_value(
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

fn default_graph_name(engine: &Engine, function_name: &str) -> Result<String, SQLError> {
    let graphs = engine
        .list_graphs()
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?;
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
    execute_tree_entries(
        engine,
        &OperatorTree::PageRank {
            graph: name.to_string(),
        },
    )
}

pub(super) fn graph_hits_entries(
    engine: &Engine,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_tree_entries(
        engine,
        &OperatorTree::HITS {
            graph: name.to_string(),
        },
    )
}

pub(super) fn graph_betweenness_entries(
    engine: &Engine,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_tree_entries(
        engine,
        &OperatorTree::BetweennessCentrality {
            graph: name.to_string(),
        },
    )
}

pub(super) fn execute_tree_entries(
    engine: &Engine,
    tree: &OperatorTree,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let posting = crate::operator_tree_bridge::expect_posting_output(
        crate::operator_tree_bridge::execute_operator_tree_in_execution(engine, "", &[], tree)?,
        "SQL table function",
    )?;
    Ok(posting
        .entries()
        .iter()
        .map(|entry| ScoredEntry {
            doc_id: entry.doc_id,
            score: entry.payload.score,
        })
        .collect())
}

pub(super) fn run_graph_create(
    engine: &Engine,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(engine);
    run_graph_create_with_evaluator(engine, args, &mut |expr| eval_scalar(expr, &ctx))?;
    Ok(Vec::new())
}

pub(super) fn run_graph_create_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<bool, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "graph_create".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let name = expect_evaluated_string(evaluate(&args[0])?, "graph_create.name")?;
    engine
        .create_graph(name)
        .map_err(|err| SQLError::Internal(format!("create graph: {err}")))
}

pub(super) fn run_graph_drop(
    engine: &Engine,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(engine);
    run_graph_drop_with_evaluator(engine, args, &mut |expr| eval_scalar(expr, &ctx))?;
    Ok(Vec::new())
}

pub(super) fn run_graph_drop_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<bool, SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "graph_drop".into(),
            expected: "1 or 2".into(),
            actual: args.len(),
        });
    }
    let name = expect_evaluated_string(evaluate(&args[0])?, "graph_drop.name")?;
    let graph_exists = engine
        .has_graph(&name)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?;
    if let Some(cascade_expr) = args.get(1) {
        match evaluate(cascade_expr)? {
            Value::Bool(true) => {}
            Value::Bool(false) if graph_exists => {
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
    engine
        .drop_graph(&name)
        .map_err(|err| SQLError::Internal(format!("drop graph: {err}")))
}

/// Apache AGE graph name validation: at least 3 characters and the
/// first character must be a letter or underscore.
fn age_graph_name_is_valid(name: &str) -> bool {
    name.len() >= 3
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

fn eval_age_graph_name_with(
    expr: &ScalarExpr,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<String, SQLError> {
    match evaluate(expr)? {
        Value::Null => Err(SQLError::Unsupported("graph name can not be NULL".into())),
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "graph name must be a string, got {other:?}"
        ))),
    }
}

/// `SELECT create_graph('name')` with AGE 1.6.0 semantics: validates
/// the name, rejects duplicates, and returns void (SQL NULL).
pub(super) fn run_age_create_graph_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "create_graph".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    if !age_graph_name_is_valid(&name) {
        return Err(SQLError::Unsupported("graph name is invalid".into()));
    }
    if engine
        .has_graph(&name)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?
    {
        return Err(SQLError::Unsupported(format!(
            "graph \"{name}\" already exists"
        )));
    }
    engine
        .create_graph(name)
        .map_err(|err| SQLError::Internal(format!("create graph: {err}")))?;
    Ok(Value::Null)
}

/// `SELECT drop_graph('name'[, cascade])` with AGE 1.6.0 semantics:
/// without `cascade => true` the drop always fails (the graph schema
/// always contains its label tables), and success returns void.
pub(super) fn run_age_drop_graph_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "drop_graph".into(),
            expected: "1 or 2".into(),
            actual: args.len(),
        });
    }
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    if !engine
        .has_graph(&name)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?
    {
        return Err(SQLError::Unsupported(format!(
            "graph \"{name}\" does not exist"
        )));
    }
    let cascade = match args.get(1) {
        Some(expr) => match evaluate(expr)? {
            Value::Bool(b) => b,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "drop_graph.cascade must be a boolean, got {other:?}"
                )));
            }
        },
        None => false,
    };
    if !cascade {
        // AGE maps this onto `DROP SCHEMA <name> RESTRICT`, which
        // always fails because the label tables live in the schema.
        return Err(SQLError::Unsupported(format!(
            "cannot drop schema {name} because other objects depend on it"
        )));
    }
    engine
        .drop_graph(&name)
        .map_err(|err| SQLError::Internal(format!("drop graph: {err}")))?;
    Ok(Value::Null)
}

fn expect_string(
    expr: &ScalarExpr,
    name: &str,
    ctx: &ScalarEvalContext,
) -> Result<String, SQLError> {
    expect_evaluated_string(eval_scalar(expr, ctx)?, name)
}

fn expect_evaluated_string(value: Value, name: &str) -> Result<String, SQLError> {
    match value {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a string, got {other:?}"
        ))),
    }
}

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

/// Reject silently-empty text searches up front: a match function whose
/// field is not a real column, or is a column without a text index,
/// previously returned zero rows with no diagnostic.
fn validate_text_match_field(
    engine: &Engine,
    table: &str,
    field: &str,
    function_name: &str,
) -> Result<(), SQLError> {
    if !engine
        .has_table(table)
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: unknown table `{table}`"
        )));
    }
    let indexed = engine
        .fts_fields_for_table(table)?
        .iter()
        .any(|fts| fts == field);
    if !indexed {
        if !engine
            .table_has_column(table, field)
            .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?
            && !engine
                .table_columns(table)
                .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?
                .is_empty()
        {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name}: column `{field}` does not exist on table `{table}`"
            )));
        }
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
    if !engine
        .has_table(table)
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: unknown table `{table}`"
        )));
    }
    if engine.fts_fields_for_table(table)?.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: table `{table}` has no text-indexed columns; \
             create one with CREATE INDEX ... ON {table} USING gin (...)"
        )));
    }
    Ok(())
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

pub(super) fn expect_column_name(expr: &ScalarExpr, label: &str) -> Result<String, SQLError> {
    match expr {
        ScalarExpr::Column(name) => Ok(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Ok(column.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be a column reference, got {other:?}"
        ))),
    }
}

fn expect_field_name_or_string(
    expr: &ScalarExpr,
    label: &str,
    ctx: &ScalarEvalContext<'_>,
) -> Result<String, SQLError> {
    match expr {
        ScalarExpr::Column(name) => Ok(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Ok(column.clone()),
        _ => expect_string(expr, label, ctx),
    }
}

fn expect_usize(
    expr: &ScalarExpr,
    label: &str,
    ctx: &ScalarEvalContext<'_>,
) -> Result<usize, SQLError> {
    let v = eval_scalar(expr, ctx)?;
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
