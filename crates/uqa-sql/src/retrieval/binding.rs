//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime argument binding and checked logical retrieval selection.

use super::{
    bind_operator_argument, checked_retrieval_call_tree_present, const_string, const_usize,
    default_operator_graph, lower_document_boolean, lower_function, lower_where,
    try_lower_attention_fusion, validate_checked_retrieval_call_tree,
    validate_operator_function_arity, validate_probability_signal_contract, BindingResult,
    RetrievalArguments, RetrievalConstants, RetrievalExpr, SQLError, ScalarExpr, Value,
};

pub fn lower_sql_function_bound(
    source: &dyn RetrievalArguments,
    name: &str,
    args: &[ScalarExpr],
    constants: &RetrievalConstants<'_>,
) -> BindingResult<RetrievalExpr> {
    validate_operator_function_arity(name, args.len())?;
    validate_probability_signal_contract(name, args)?;
    let mut bound = args
        .iter()
        .map(|argument| bind_operator_argument(source, argument, constants))
        .collect::<Result<Vec<_>, _>>()?;
    match name.to_ascii_lowercase().as_str() {
        "rpq" if bound.len() == 2 => bound.push(ScalarExpr::Literal(Value::Str(
            default_operator_graph(source, "rpq")?,
        ))),
        "graph_pagerank" | "pagerank" | "graph_hits" | "hits" | "graph_betweenness"
        | "betweenness"
            if bound.is_empty() =>
        {
            bound.push(ScalarExpr::Literal(Value::Str(default_operator_graph(
                source, name,
            )?)));
        }
        _ => {}
    }
    let empty_constants = constants.without_parameters();
    validate_checked_retrieval_call_tree(name, &bound, &empty_constants)?;
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "attention" | "fuse_attention" | "fuse_multihead"
    ) {
        return try_lower_attention_fusion(name, &bound, &empty_constants);
    }
    lower_function(name, &bound, &empty_constants).ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{name} arguments cannot be lowered to the shared operator IR"
        ))
    })
}

fn centrality_kind(name: &str) -> Option<&'static str> {
    match name {
        "graph_pagerank" | "pagerank" => Some("pagerank"),
        "graph_hits" | "hits" => Some("hits"),
        "graph_betweenness" | "betweenness" => Some("betweenness"),
        _ => None,
    }
}

fn lower_bound_centrality(
    source: &dyn RetrievalArguments,
    name: &str,
    args: &[ScalarExpr],
    kind: &str,
) -> BindingResult<RetrievalExpr> {
    let graph = match args {
        [] => default_operator_graph(source, name)?,
        [_] => {
            return Err(SQLError::TypeMismatch(format!(
                "{name}.graph must be a constant string"
            )))
        }
        _ => {
            return Err(SQLError::BadArity {
                name: name.to_string(),
                expected: "0..=1".into(),
                actual: args.len(),
            })
        }
    };
    Ok(match kind {
        "pagerank" => RetrievalExpr::PageRank { graph },
        "hits" => RetrievalExpr::HITS { graph },
        _ => RetrievalExpr::BetweennessCentrality { graph },
    })
}

fn lower_bound_rpq(
    source: &dyn RetrievalArguments,
    args: &[ScalarExpr],
    constants: &RetrievalConstants<'_>,
) -> BindingResult<RetrievalExpr> {
    let graph = default_operator_graph(source, "rpq")?;
    let rpq_source = const_string(&args[0], constants)
        .ok_or_else(|| SQLError::TypeMismatch("rpq.expr must be a constant string".into()))?;
    let start_vertex = const_usize(&args[1], constants)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| SQLError::TypeMismatch("rpq.start must be a non-negative integer".into()))?;
    Ok(RetrievalExpr::RegularPathQuery {
        rpq_source,
        start_vertex,
        graph,
    })
}

fn lower_bound_function(
    source: &dyn RetrievalArguments,
    name: &str,
    args: &[ScalarExpr],
    constants: &RetrievalConstants<'_>,
) -> BindingResult<Option<RetrievalExpr>> {
    validate_operator_function_arity(name, args.len())?;
    validate_probability_signal_contract(name, args)?;

    let bound;
    let empty_constants = constants.without_parameters();
    let (lowering_args, lowering_constants): (&[ScalarExpr], &RetrievalConstants<'_>) =
        if checked_retrieval_call_tree_present(name, args) {
            bound = args
                .iter()
                .map(|argument| bind_operator_argument(source, argument, constants))
                .collect::<Result<Vec<_>, _>>()?;
            (&bound, &empty_constants)
        } else {
            (args, constants)
        };
    validate_checked_retrieval_call_tree(name, lowering_args, lowering_constants)?;

    if let Some(tree) = lower_function(name, lowering_args, lowering_constants) {
        return Ok(Some(tree));
    }
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "attention" | "fuse_attention" | "fuse_multihead"
    ) {
        return try_lower_attention_fusion(name, lowering_args, lowering_constants).map(Some);
    }
    let lower_name = name.to_ascii_lowercase();
    if let Some(kind) = centrality_kind(&lower_name) {
        return lower_bound_centrality(source, name, lowering_args, kind).map(Some);
    }
    if lower_name == "rpq" && lowering_args.len() == 2 {
        return lower_bound_rpq(source, lowering_args, lowering_constants).map(Some);
    }
    if matches!(
        lower_name.as_str(),
        "graph_traverse"
            | "traverse_match"
            | "graph_neighbors"
            | "graph_edges"
            | "temporal_traverse"
            | "rpq"
            | "deep_predict"
    ) {
        return Err(SQLError::TypeMismatch(format!(
            "{name} arguments must be execution-time constants of the documented types"
        )));
    }
    Ok(None)
}

pub fn lower_where_bound(
    source: &dyn RetrievalArguments,
    expression: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
) -> Result<Option<RetrievalExpr>, SQLError> {
    match expression {
        ScalarExpr::And(parts) => {
            let mut children = Vec::with_capacity(parts.len());
            for part in parts {
                let Some(child) = lower_where_bound(source, part, constants)? else {
                    return Ok(None);
                };
                children.push(child);
            }
            Ok(Some(lower_document_boolean(children, false)))
        }
        ScalarExpr::Or(parts) => {
            let mut children = Vec::with_capacity(parts.len());
            for part in parts {
                let Some(child) = lower_where_bound(source, part, constants)? else {
                    return Ok(None);
                };
                children.push(child);
            }
            Ok(Some(lower_document_boolean(children, true)))
        }
        ScalarExpr::Not(inner) if crate::semantics::expr_is_null_free(inner) => {
            Ok(lower_where_bound(source, inner, constants)?
                .map(|child| RetrievalExpr::Complement(Box::new(child))))
        }
        ScalarExpr::Func { name, args, .. } => lower_bound_function(source, name, args, constants),
        _ => Ok(lower_where(expression, constants)),
    }
}
