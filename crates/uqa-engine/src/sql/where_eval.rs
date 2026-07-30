//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mixed SQL WHERE evaluation for boolean filters and row-emitting search functions.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::DocId;
use uqa_execution::ScalarExpr;
use uqa_sql::{SQLError, SQLParam};

use crate::{Engine, ScoredEntry};

use super::row_functions::execute_function;
use super::select::{execute_filter_rows, CteScope};

fn filter_documents(
    engine: &Engine,
    filter: &ScalarExpr,
    params: &[SQLParam],
    documents: Vec<uqa_storage::document_store::Document>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let scope = CteScope::new();
    let rows = execute_filter_rows(engine, documents, filter.clone(), params, &scope)?;
    rows.into_iter()
        .map(|row| match row.get(super::DOC_ID_COLUMN) {
            Some(uqa_core::Value::Int(doc_id)) if *doc_id >= 0 => Ok(ScoredEntry {
                doc_id: *doc_id as DocId,
                score: 0.0,
            }),
            _ => Err(SQLError::Internal(
                "physical table filter lost its document id".into(),
            )),
        })
        .collect()
}

fn filter_table_rows(
    engine: &Engine,
    table: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let doc_ids = engine.table_doc_ids(table)?;
    // When the predicate reads a known column set, evaluate it against
    // a per-row field projection fetched in one storage scan instead
    // of materialising every document.
    let mut columns = std::collections::BTreeSet::new();
    if filter.collect_columns(&mut columns) {
        let names: Vec<String> = columns.into_iter().collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let field_values = engine.get_document_fields_multi(table, &doc_ids, &refs)?;
        let mut documents = Vec::with_capacity(doc_ids.len());
        for doc_id in doc_ids {
            let values = field_values.get(&doc_id).ok_or_else(|| {
                SQLError::Internal(format!(
                    "WHERE scan: document {doc_id} listed by table `{table}` disappeared during the statement"
                ))
            })?;
            let mut document = uqa_storage::document_store::Document::new();
            for (name, value) in names.iter().zip(values) {
                document.insert(name.clone(), value.clone());
            }
            document.insert(super::DOC_ID_COLUMN.into(), super::doc_id_value(doc_id)?);
            documents.push(document);
        }
        return filter_documents(engine, filter, params, documents);
    }
    let mut documents = Vec::with_capacity(doc_ids.len());
    for doc_id in doc_ids {
        let mut document = engine.get_document(table, doc_id)?.ok_or_else(|| {
            SQLError::Internal(format!(
                "WHERE scan: document {doc_id} listed by table `{table}` disappeared during the statement"
            ))
        })?;
        document.insert(super::DOC_ID_COLUMN.into(), super::doc_id_value(doc_id)?);
        documents.push(document);
    }
    filter_documents(engine, filter, params, documents)
}

pub(super) fn execute_mixed_where(
    engine: &Engine,
    table: &str,
    filter: &ScalarExpr,
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
    filter: &ScalarExpr,
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
            ScalarExpr::Func { name, args, .. } if uqa_sql::registry::is_registered(name) => {
                execute_function(engine, table, name, args, params)?
            }
            other => execute_mixed_where(engine, table, other, params)?,
        }
    };
    Ok(scored.into_iter().map(|entry| entry.doc_id).collect())
}

fn is_jsonpath_fts_match_filter(filter: &ScalarExpr) -> bool {
    match filter {
        ScalarExpr::Func { name, args, .. } => is_jsonpath_fts_match(name, args),
        _ => false,
    }
}

fn execute_mixed_where_expr(
    engine: &Engine,
    table: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    match filter {
        ScalarExpr::And(parts) => {
            let mut iter = parts.iter();
            let Some(first) = iter.next() else {
                return all_table_rows(engine, table);
            };
            let mut out = execute_mixed_where_expr(engine, table, first, params)?;
            for part in iter {
                let rhs = execute_mixed_where_expr(engine, table, part, params)?;
                out = intersect_scored(out, rhs);
            }
            Ok(out)
        }
        ScalarExpr::Or(parts) => {
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
        ScalarExpr::Not(inner) if expr_is_null_free(inner) => complement_scored(
            engine,
            table,
            execute_mixed_where_expr(engine, table, inner, params)?,
        ),
        ScalarExpr::Func { name, args, .. } if uqa_sql::registry::is_registered(name) => {
            if is_jsonpath_fts_match(name, args) {
                filter_table_rows(engine, table, filter, params)
            } else {
                execute_function(engine, table, name, args, params)
            }
        }
        other => filter_table_rows(engine, table, other, params),
    }
}

fn is_jsonpath_fts_match(name: &str, args: &[ScalarExpr]) -> bool {
    name.eq_ignore_ascii_case("fts_match")
        && matches!(
            args.get(1),
            Some(ScalarExpr::Literal(uqa_core::Value::Str(path))) if path.trim_start().starts_with('$')
        )
}

/// True when the expression can never evaluate to SQL NULL for any
/// row: registered search functions, IS NULL tests, and boolean
/// combinations thereof. Anything referencing column comparisons may
/// yield NULL, so set-complement `NOT` would be unsound for it.
pub(crate) fn expr_is_null_free(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Func { name, .. } => uqa_sql::registry::is_registered(name),
        ScalarExpr::IsNull { .. } => true,
        ScalarExpr::Exists { .. } => true,
        ScalarExpr::Literal(v) => !matches!(v, uqa_core::Value::Null),
        ScalarExpr::And(parts) | ScalarExpr::Or(parts) => parts.iter().all(expr_is_null_free),
        ScalarExpr::Not(inner) => expr_is_null_free(inner),
        _ => false,
    }
}

fn all_table_rows(engine: &Engine, table: &str) -> Result<Vec<ScoredEntry>, SQLError> {
    Ok(engine
        .table_doc_ids(table)?
        .into_iter()
        .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
        .collect())
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

fn complement_scored(
    engine: &Engine,
    table: &str,
    rows: Vec<ScoredEntry>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let excluded: BTreeSet<DocId> = rows.into_iter().map(|e| e.doc_id).collect();
    Ok(engine
        .table_doc_ids(table)?
        .into_iter()
        .filter(|doc_id| !excluded.contains(doc_id))
        .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
        .collect())
}
