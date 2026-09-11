//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph table-function plans and their physical result rows.

use super::{context::TableFunctionContext, TableFunctionCall, TableFunctionRows};
use crate::operator_tree::runtime::{execute_tree, expect_posting_output, TreeExecutionContext};
use uqa_core::{ScoredEntry, Value};
use uqa_operators::OperatorTree;
use uqa_sql::{
    semantics::{doc_id_value, graph_functions},
    SQLError,
};

pub(super) fn build_rows(
    context: &TableFunctionContext<'_>,
    call: TableFunctionCall<'_>,
    lower: &str,
    evaluated: &[Value],
) -> Result<TableFunctionRows, SQLError> {
    let TableFunctionCall {
        args,
        column_aliases,
        column_types,
        ..
    } = call;
    let mut out = Vec::new();
    match lower {
        "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
        | "graph_betweenness" => {
            let graph = graph_functions::centrality_graph(context.graph_names, evaluated, lower)?;
            let entries = match lower {
                "pagerank" | "graph_pagerank" => {
                    graph_pagerank_entries(&context.retrieval, &graph)?
                }
                "hits" | "graph_hits" => graph_hits_entries(&context.retrieval, &graph)?,
                "betweenness" | "graph_betweenness" => {
                    graph_betweenness_entries(&context.retrieval, &graph)?
                }
                _ => {
                    return Err(SQLError::Internal(format!(
                        "graph centrality function `{lower}` reached an unsupported dispatch branch"
                    )));
                }
            };
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "_doc_id".into());
            let score_col = column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "_score".into());
            for entry in entries {
                out.push(vec![doc_id_value(entry.doc_id)?, Value::Float(entry.score)]);
            }
            Ok(TableFunctionRows::materialized(
                vec![id_col, score_col],
                out,
            ))
        }
        "cypher" => Ok(TableFunctionRows::materialized(
            column_aliases.to_vec(),
            crate::query::cypher::build_rows(
                context.cypher,
                args,
                evaluated,
                column_aliases,
                column_types,
            )?,
        )),
        "rpq" => {
            let (expr_str, start, graph) =
                graph_functions::regular_path_arguments(context.graph_names, evaluated)?;
            let entries = execute_tree_entries(
                &context.retrieval,
                &OperatorTree::RegularPathQuery {
                    rpq_source: expr_str,
                    start_vertex: start,
                    graph,
                },
            )?;
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "vertex_id".into());
            for entry in entries {
                out.push(vec![doc_id_value(entry.doc_id)?]);
            }
            Ok(TableFunctionRows::materialized(vec![id_col], out))
        }
        _ => unreachable!("graph table function selected by the caller"),
    }
}

fn graph_pagerank_entries(
    context: &TreeExecutionContext<'_>,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_tree_entries(
        context,
        &OperatorTree::PageRank {
            graph: name.to_string(),
        },
    )
}

fn graph_hits_entries(
    context: &TreeExecutionContext<'_>,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_tree_entries(
        context,
        &OperatorTree::HITS {
            graph: name.to_string(),
        },
    )
}

fn graph_betweenness_entries(
    context: &TreeExecutionContext<'_>,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_tree_entries(
        context,
        &OperatorTree::BetweennessCentrality {
            graph: name.to_string(),
        },
    )
}

fn execute_tree_entries(
    context: &TreeExecutionContext<'_>,
    tree: &OperatorTree,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let posting = expect_posting_output(
        execute_tree(context, "", "", &[], tree)?,
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
