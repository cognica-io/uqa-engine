//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mixed SQL WHERE evaluation for boolean filters and row-emitting search functions.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::DocId;
use uqa_sql::ast::Expr;
use uqa_sql::{SQLError, SQLParam};

use crate::{Engine, ScoredEntry};

use super::row_functions::execute_function;

fn filter_table_rows(
    engine: &Engine,
    table: &str,
    filter: &Expr,
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let doc_ids = engine.table_doc_ids(table);
    // When the predicate reads a known column set, evaluate it against
    // a per-row field projection fetched in one storage scan instead
    // of materialising every document.
    let mut columns = std::collections::BTreeSet::new();
    if super::aggregates::collect_expr_columns(filter, &mut columns) {
        let names: Vec<String> = columns.into_iter().collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let field_values = engine.get_document_fields_multi(table, &doc_ids, &refs);
        let mut out = Vec::new();
        let empty: Vec<uqa_core::Value> = Vec::new();
        for doc_id in doc_ids {
            let values = field_values.get(&doc_id).unwrap_or(&empty);
            let mut document = uqa_storage::document_store::Document::new();
            for (name, value) in names.iter().zip(values) {
                document.insert(name.clone(), value.clone());
            }
            let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params).with_engine(engine);
            let v = uqa_sql::expr::eval(filter, &ctx)?;
            if uqa_sql::expr::truthy(&v) {
                out.push(ScoredEntry { doc_id, score: 0.0 });
            }
        }
        return Ok(out);
    }
    let mut out = Vec::new();
    for doc_id in doc_ids {
        let document = engine.get_document(table, doc_id).unwrap_or_default();
        let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params).with_engine(engine);
        let v = uqa_sql::expr::eval(filter, &ctx)?;
        if uqa_sql::expr::truthy(&v) {
            out.push(ScoredEntry { doc_id, score: 0.0 });
        }
    }
    Ok(out)
}

pub(super) fn execute_mixed_where(
    engine: &Engine,
    table: &str,
    filter: &Expr,
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let mut rows = execute_mixed_where_expr(engine, table, filter, params)?;
    rows.sort_by_key(|e| e.doc_id);
    Ok(rows)
}

/// Resolve a WHERE clause to the matching doc ids with the same
/// machinery single-table SELECT uses: the operator-tree pipeline
/// (value indexes, posting lists) first, then registered row
/// functions, then the evaluated scan. UPDATE / DELETE call this so
/// point writes stop paying a full table materialisation.
pub(super) fn collect_where_doc_ids(
    engine: &Engine,
    table: &str,
    filter: &Expr,
    params: &[SQLParam],
) -> Result<Vec<DocId>, SQLError> {
    let scored = if is_jsonpath_fts_match_filter(filter) {
        execute_mixed_where(engine, table, filter, params)?
    } else if let Some(entries) =
        crate::operator_tree_bridge::run_optimised(engine, table, Some(filter), params)?
    {
        entries
    } else {
        match filter {
            Expr::Func { name, args, .. } if uqa_sql::registry::is_registered(name) => {
                execute_function(engine, table, name, args, params)?
            }
            other => execute_mixed_where(engine, table, other, params)?,
        }
    };
    Ok(scored.into_iter().map(|entry| entry.doc_id).collect())
}

fn is_jsonpath_fts_match_filter(filter: &Expr) -> bool {
    match filter {
        Expr::Func { name, args, .. } => is_jsonpath_fts_match(name, args),
        _ => false,
    }
}

fn execute_mixed_where_expr(
    engine: &Engine,
    table: &str,
    filter: &Expr,
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    match filter {
        Expr::And(parts) => {
            let mut iter = parts.iter();
            let Some(first) = iter.next() else {
                return Ok(all_table_rows(engine, table));
            };
            let mut out = execute_mixed_where_expr(engine, table, first, params)?;
            for part in iter {
                let rhs = execute_mixed_where_expr(engine, table, part, params)?;
                out = intersect_scored(out, rhs);
            }
            Ok(out)
        }
        Expr::Or(parts) => {
            let mut out = Vec::new();
            for part in parts {
                out = union_scored(out, execute_mixed_where_expr(engine, table, part, params)?);
            }
            Ok(out)
        }
        // NOT is only a set complement when the inner predicate cannot
        // evaluate to NULL for any row (search functions produce
        // definite match sets). Column predicates go through the
        // row evaluator so `NOT (col = 5)` keeps SQL three-valued
        // semantics: rows where `col` is NULL match neither side.
        Expr::Not(inner) if expr_is_null_free(inner) => Ok(complement_scored(
            engine,
            table,
            execute_mixed_where_expr(engine, table, inner, params)?,
        )),
        Expr::Func { name, args, .. } if uqa_sql::registry::is_registered(name) => {
            if is_jsonpath_fts_match(name, args) {
                filter_table_rows(engine, table, filter, params)
            } else {
                execute_function(engine, table, name, args, params)
            }
        }
        other => filter_table_rows(engine, table, other, params),
    }
}

fn is_jsonpath_fts_match(name: &str, args: &[Expr]) -> bool {
    name.eq_ignore_ascii_case("fts_match")
        && matches!(
            args.get(1),
            Some(Expr::Literal(uqa_core::Value::Str(path))) if path.trim_start().starts_with('$')
        )
}

/// True when the expression can never evaluate to SQL NULL for any
/// row: registered search functions, IS NULL tests, and boolean
/// combinations thereof. Anything referencing column comparisons may
/// yield NULL, so set-complement `NOT` would be unsound for it.
pub(crate) fn expr_is_null_free(expr: &Expr) -> bool {
    match expr {
        Expr::Func { name, .. } => uqa_sql::registry::is_registered(name),
        Expr::IsNull { .. } => true,
        Expr::Exists { .. } => true,
        Expr::Literal(v) => !matches!(v, uqa_core::Value::Null),
        Expr::And(parts) | Expr::Or(parts) => parts.iter().all(expr_is_null_free),
        Expr::Not(inner) => expr_is_null_free(inner),
        _ => false,
    }
}

fn all_table_rows(engine: &Engine, table: &str) -> Vec<ScoredEntry> {
    engine
        .table_doc_ids(table)
        .into_iter()
        .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
        .collect()
}

fn intersect_scored(left: Vec<ScoredEntry>, right: Vec<ScoredEntry>) -> Vec<ScoredEntry> {
    let right_scores: BTreeMap<DocId, f64> =
        right.into_iter().map(|e| (e.doc_id, e.score)).collect();
    let mut out = Vec::new();
    for entry in left {
        if let Some(rhs) = right_scores.get(&entry.doc_id) {
            out.push(ScoredEntry {
                doc_id: entry.doc_id,
                score: entry.score + rhs,
            });
        }
    }
    out
}

fn union_scored(left: Vec<ScoredEntry>, right: Vec<ScoredEntry>) -> Vec<ScoredEntry> {
    let mut scores: BTreeMap<DocId, f64> = BTreeMap::new();
    for entry in left.into_iter().chain(right) {
        *scores.entry(entry.doc_id).or_insert(0.0) += entry.score;
    }
    scores
        .into_iter()
        .map(|(doc_id, score)| ScoredEntry { doc_id, score })
        .collect()
}

fn complement_scored(engine: &Engine, table: &str, rows: Vec<ScoredEntry>) -> Vec<ScoredEntry> {
    let excluded: BTreeSet<DocId> = rows.into_iter().map(|e| e.doc_id).collect();
    engine
        .table_doc_ids(table)
        .into_iter()
        .filter(|doc_id| !excluded.contains(doc_id))
        .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
        .collect()
}
