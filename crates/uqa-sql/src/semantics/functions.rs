//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function naming, shape, and relation-field argument semantics.

use crate::registry::FunctionKind;
use crate::{SQLError, ScalarExpr};
use std::sync::LazyLock;
use uqa_core::Value;

/// Executor-only carrier for `PostgreSQL` 18's `merge_action()` value. The attribute has no SQL name and therefore cannot collide with a target or source column named `_merge_action`.
pub fn merge_action_attribute() -> crate::ast::InternalColumnRef {
    static ATTRIBUTE: LazyLock<crate::ast::InternalColumnRef> =
        LazyLock::new(|| crate::ast::InternalRelationId::allocate().column(0));
    *ATTRIBUTE
}
/// Resolve reserved system-schema aliases only when the local name belongs to
/// that schema's built-in surface. Ordinary qualified names stay intact for
/// runtime callbacks and user-defined routine lookup.
pub fn builtin_function_dispatch_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let Some((schema, local)) = lower.split_once('.') else {
        return lower;
    };
    let is_builtin = match schema {
        "ag_catalog" => matches!(
            local,
            "cypher"
                | "create_graph"
                | "drop_graph"
                | "graph_exists"
                | "create_vlabel"
                | "create_elabel"
                | "drop_label"
                | "alter_graph"
        ),
        "pg_catalog" => {
            crate::registry::is_registered(local)
                || matches!(
                    local,
                    "generate_series"
                        | "unnest"
                        | "regexp_split_to_table"
                        | "string_to_table"
                        | "json_array_elements"
                        | "jsonb_array_elements"
                        | "json_array_elements_text"
                        | "jsonb_array_elements_text"
                        | "json_each"
                        | "jsonb_each"
                        | "json_each_text"
                        | "jsonb_each_text"
                        | "json_object_keys"
                        | "jsonb_object_keys"
                        | "upper"
                        | "lower"
                        | "bit_length"
                        | "char_length"
                        | "character_length"
                        | "crc32"
                        | "crc32c"
                        | "gamma"
                        | "json_strip_nulls"
                        | "jsonb_strip_nulls"
                        | "length"
                        | "lgamma"
                        | "md5"
                        | "octet_length"
                        | "reverse"
                        | "random"
                        | "setseed"
                        | "nextval"
                        | "currval"
                        | "lastval"
                        | "setval"
                        | "current_schema"
                        | "current_schemas"
                        | "pg_backend_pid"
                        | "pg_listening_channels"
                        | "pg_notify"
                        | "pg_notification_queue_usage"
                        | "pg_get_expr"
                        | "pg_get_partkeydef"
                        | "pg_get_serial_sequence"
                        | "pg_get_triggerdef"
                        | "pg_get_ruledef"
                        | "pg_get_viewdef"
                        | "pg_get_indexdef"
                        | "format_type"
                        | "pg_has_role"
                        | "has_table_privilege"
                        | "has_column_privilege"
                        | "has_database_privilege"
                        | "has_schema_privilege"
                        | "has_sequence_privilege"
                )
        }
        _ => false,
    };
    if is_builtin {
        local.to_string()
    } else {
        lower
    }
}

pub fn is_builtin_aggregate(expr: &ScalarExpr) -> bool {
    matches!(expr, ScalarExpr::Func { name, .. } if matches!(
        name.to_ascii_lowercase().as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "string_agg"
            | "array_agg"
            | "bool_and"
            | "bool_or"
            | "stddev"
            | "stddev_samp"
            | "stddev_pop"
            | "variance"
            | "var_samp"
            | "var_pop"
            | "percentile_cont"
            | "percentile_disc"
            | "mode"
            | "json_agg"
            | "jsonb_agg"
            | "json_object_agg"
            | "jsonb_object_agg"
    ))
}

pub enum MultiFieldMatchShape<'a> {
    FieldsThenQuery {
        fields: Vec<&'a ScalarExpr>,
        query_idx: usize,
    },
    Pairs {
        fields: Vec<&'a ScalarExpr>,
    },
}

pub fn multi_field_match_shape(args: &[ScalarExpr]) -> Result<MultiFieldMatchShape<'_>, SQLError> {
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
    if args.len() < 4 || !args.len().is_multiple_of(2) {
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

/// Return whether a registered function consumes this argument as a relation-field identifier rather than as a scalar row value.
pub fn is_semantic_field_argument(
    function: &str,
    args: &[ScalarExpr],
    argument_index: usize,
) -> Result<bool, SQLError> {
    let dispatch_name = crate::semantics::builtin_function_dispatch_name(function);
    let Some(kind) = crate::registry::lookup(&dispatch_name) else {
        return Ok(false);
    };
    let is_field = match kind {
        FunctionKind::TextMatch | FunctionKind::BayesianMatch | FunctionKind::KNNMatch => {
            argument_index == 0
        }
        FunctionKind::FTSMatch => argument_index == 0 && !fts_query_is_jsonpath(args.get(1)),
        FunctionKind::BayesianMatchWithPrior => matches!(argument_index, 0 | 2),
        FunctionKind::CalibratedVectorMatch => argument_index == 0,
        FunctionKind::MultiFieldMatch => match multi_field_match_shape(args)? {
            MultiFieldMatchShape::FieldsThenQuery { query_idx, .. } => argument_index < query_idx,
            MultiFieldMatchShape::Pairs { .. } => argument_index.is_multiple_of(2),
        },
        FunctionKind::StagedRetrieval => {
            !matches!(args.first(), Some(ScalarExpr::Func { .. }))
                && argument_index.is_multiple_of(3)
        }
        FunctionKind::UQAFacets => true,
        FunctionKind::ScoreBM25 | FunctionKind::ScoreBayesianBM25 => {
            args.len() == 2 && argument_index == 0
        }
        FunctionKind::FuseLogOdds
        | FunctionKind::PositiveEvidencePool
        | FunctionKind::BayesianEvidenceFusion
        | FunctionKind::GraphPagerank
        | FunctionKind::GraphHits
        | FunctionKind::GraphBetweenness
        | FunctionKind::GraphTraverse
        | FunctionKind::GraphNeighbors
        | FunctionKind::DeepPredict
        | FunctionKind::UQAHighlight
        | FunctionKind::TraverseMatch
        | FunctionKind::TemporalTraverse
        | FunctionKind::RPQ
        | FunctionKind::GraphCreate
        | FunctionKind::GraphDrop
        | FunctionKind::GraphExists
        | FunctionKind::GraphLabelCreate
        | FunctionKind::GraphLabelDrop
        | FunctionKind::GraphAlter
        | FunctionKind::GraphEdges
        | FunctionKind::AttentionFusion
        | FunctionKind::LearnedFusion
        | FunctionKind::SparseThreshold
        | FunctionKind::DeepLearn
        | FunctionKind::Convolve
        | FunctionKind::Pool
        | FunctionKind::Flatten
        | FunctionKind::Dense
        | FunctionKind::Softmax
        | FunctionKind::Layer
        | FunctionKind::Model => false,
    };
    Ok(is_field)
}

/// The `@@` operator doubles as a `JSONPath` match when the right-hand
/// side is a `$...` path literal; that form evaluates row-level JSON and
/// needs no text index.
pub fn fts_query_is_jsonpath(query_arg: Option<&ScalarExpr>) -> bool {
    matches!(
        query_arg,
        Some(ScalarExpr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')
    )
}

/// Whether a scalar expression contains a posting-list retrieval operator.
/// Relational executors use the same classification as access-path planning
/// so registered retrieval calls never fall through to scalar evaluation.
pub fn contains_retrieval(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            retrieval_function(name)
                || args.iter().any(contains_retrieval)
                || order_by.iter().any(|order| contains_retrieval(&order.expr))
                || filter.as_deref().is_some_and(contains_retrieval)
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(contains_retrieval),
        ScalarExpr::Binary { lhs, rhs, .. } => contains_retrieval(lhs) || contains_retrieval(rhs),
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => contains_retrieval(inner),
        ScalarExpr::Between { expr, low, high } => {
            contains_retrieval(expr) || contains_retrieval(low) || contains_retrieval(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            contains_retrieval(expr) || list.iter().any(contains_retrieval)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(contains_retrieval)
                || spec.partition_by.iter().any(contains_retrieval)
                || spec
                    .order_by
                    .iter()
                    .any(|order| contains_retrieval(&order.expr))
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(contains_retrieval)
                || when.iter().any(|(condition, result)| {
                    contains_retrieval(condition) || contains_retrieval(result)
                })
                || else_branch.as_deref().is_some_and(contains_retrieval)
        }
        ScalarExpr::InSubquery { expr, .. } => contains_retrieval(expr),
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

pub fn retrieval_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "text_match"
            | "bayesian_match"
            | "fts_match"
            | "bayesian_match_with_prior"
            | "calibrated_vector_match"
            | "knn_match"
            | "fuse_log_odds"
            | "pool_positive_evidence"
            | "fuse_bayesian_evidence"
            | "multi_field_match"
            | "staged_retrieval"
            | "attention"
            | "fuse_attention"
            | "fuse_multihead"
            | "learned_fusion"
            | "fuse_learned"
            | "sparse_threshold"
            | "graph_pagerank"
            | "pagerank"
            | "graph_hits"
            | "hits"
            | "graph_betweenness"
            | "betweenness"
            | "graph_traverse"
            | "traverse_match"
            | "graph_neighbors"
            | "graph_edges"
            | "temporal_traverse"
            | "rpq"
            | "deep_predict"
    )
}

pub fn expect_column_name(expr: &ScalarExpr, label: &str) -> Result<String, SQLError> {
    match expr {
        ScalarExpr::Column(name) => Ok(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Ok(column.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be a column reference, got {other:?}"
        ))),
    }
}
