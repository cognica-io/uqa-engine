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
use super::select::{execute_filter_physical_rows, CteScope};

struct DocumentFilterInput {
    schema: uqa_execution::RowSchema,
    document_id: uqa_sql::ast::InternalColumnRef,
    doc_ids: Vec<DocId>,
    documents: Vec<uqa_storage::document_store::Document>,
}

fn filter_documents(
    engine: &Engine,
    input: DocumentFilterInput,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let DocumentFilterInput {
        schema,
        document_id,
        doc_ids,
        documents,
    } = input;
    // The caller's scope must be used rather than a fresh one: it carries the
    // CTE rows and the scalar-subquery plans this predicate may reference. A
    // fresh scope leaves those slots empty, so a residual predicate combining
    // a retrieval function with `IN (SELECT ...)`, `EXISTS`, or a scalar
    // subquery would fail to resolve slot 0.
    if documents.len() != doc_ids.len() {
        return Err(SQLError::Internal(
            "physical table filter document/id width mismatch".into(),
        ));
    }
    let rows = documents
        .into_iter()
        .zip(doc_ids)
        .map(|(document, doc_id)| {
            let mut values = schema
                .columns()
                .iter()
                .map(|column| {
                    document
                        .get(column)
                        .cloned()
                        .unwrap_or(uqa_core::Value::Null)
                })
                .collect::<Vec<_>>();
            values.push(super::doc_id_value(doc_id)?);
            Ok(uqa_execution::PhysicalRow::from_values(values))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let rows = execute_filter_physical_rows(engine, schema, rows, filter.clone(), params, ctes)?;
    rows.into_iter()
        .map(|row| {
            match row
                .schema
                .internal_slot(document_id)
                .and_then(|slot| row.physical_value_at(slot))
            {
                Some(uqa_core::Value::Int(doc_id)) if *doc_id >= 0 => Ok(ScoredEntry {
                    doc_id: *doc_id as DocId,
                    score: 0.0,
                }),
                _ => Err(SQLError::Internal(
                    "physical table filter lost its document id".into(),
                )),
            }
        })
        .collect()
}

fn filter_table_rows(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let doc_ids = engine.table_doc_ids(table)?;
    // When the predicate reads a known column set, evaluate it against
    // a per-row field projection fetched in one storage scan instead
    // of materialising every document.
    let mut columns = std::collections::BTreeSet::new();
    if filter.collect_columns(&mut columns) {
        let names: Vec<String> = columns.into_iter().collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let field_values = engine.get_query_document_fields_multi(table, &doc_ids, &refs)?;
        let mut documents = Vec::with_capacity(doc_ids.len());
        for &doc_id in &doc_ids {
            let values = field_values.get(&doc_id).ok_or_else(|| {
                SQLError::Internal(format!(
                    "WHERE scan: document {doc_id} listed by table `{table}` disappeared during the statement"
                ))
            })?;
            let mut document = uqa_storage::document_store::Document::new();
            for (name, value) in names.iter().zip(values) {
                document.insert(name.clone(), value.clone());
            }
            documents.push(document);
        }
        let (schema, document_id) = table_filter_schema(engine, table, qualifier, names)?;
        return filter_documents(
            engine,
            DocumentFilterInput {
                schema,
                document_id,
                doc_ids,
                documents,
            },
            filter,
            params,
            ctes,
        );
    }
    let mut documents = Vec::with_capacity(doc_ids.len());
    for &doc_id in &doc_ids {
        let document = engine.get_query_document(table, doc_id)?.ok_or_else(|| {
            SQLError::Internal(format!(
                "WHERE scan: document {doc_id} listed by table `{table}` disappeared during the statement"
            ))
        })?;
        documents.push(document);
    }
    let columns = engine.try_query_table_columns(table).map_err(|error| {
        SQLError::Internal(format!("read table columns for `{table}`: {error}"))
    })?;
    let (schema, document_id) = table_filter_schema(engine, table, qualifier, columns)?;
    filter_documents(
        engine,
        DocumentFilterInput {
            schema,
            document_id,
            doc_ids,
            documents,
        },
        filter,
        params,
        ctes,
    )
}

fn table_filter_schema(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    columns: Vec<String>,
) -> Result<(uqa_execution::RowSchema, uqa_sql::ast::InternalColumnRef), SQLError> {
    let definitions = engine
        .try_describe_query_table(table)
        .map_err(|error| SQLError::Internal(format!("read table schema for `{table}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    let identities = columns
        .iter()
        .map(|column| uqa_execution::ColumnIdentity::qualified(qualifier, column))
        .collect::<Vec<_>>();
    let document_id = uqa_sql::ast::InternalRelationId::allocate().column(0);
    let schema = uqa_execution::RowSchema::with_identities(columns, identities, types);
    let schema = uqa_execution::RowSchema::append_internal_typed(
        &schema,
        &[(document_id, Some(uqa_sql::ast::ColumnType::BigInteger))],
    );
    Ok((schema, document_id))
}

pub(super) fn execute_mixed_where(
    engine: &Engine,
    table: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let mut rows = execute_mixed_where_expr(engine, table, table, filter, params, ctes)?;
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
    qualifier: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<DocId>, SQLError> {
    let scored = if is_jsonpath_fts_match_filter(filter) {
        execute_mixed_where_expr(engine, table, qualifier, filter, params, ctes)?
    } else if let Some(entries) =
        crate::operator_tree_bridge::run_optimised(engine, table, Some(filter), params)?
    {
        entries
    } else {
        match filter {
            ScalarExpr::Func { name, args, .. } if uqa_sql::registry::is_registered(name) => {
                execute_function(engine, table, name, args, params)?
            }
            other => execute_mixed_where_expr(engine, table, qualifier, other, params, ctes)?,
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
    qualifier: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ScoredEntry>, SQLError> {
    match filter {
        ScalarExpr::And(parts) => {
            let mut iter = parts.iter();
            let Some(first) = iter.next() else {
                return all_table_rows(engine, table);
            };
            let mut out = execute_mixed_where_expr(engine, table, qualifier, first, params, ctes)?;
            for part in iter {
                let rhs = execute_mixed_where_expr(engine, table, qualifier, part, params, ctes)?;
                out = intersect_scored(out, rhs);
            }
            Ok(out)
        }
        ScalarExpr::Or(parts) => {
            let mut out = Vec::new();
            for part in parts {
                out = union_scored(
                    out,
                    execute_mixed_where_expr(engine, table, qualifier, part, params, ctes)?,
                );
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
            execute_mixed_where_expr(engine, table, qualifier, inner, params, ctes)?,
        ),
        ScalarExpr::Func { name, args, .. } if uqa_sql::registry::is_registered(name) => {
            if is_jsonpath_fts_match(name, args) {
                filter_table_rows(engine, table, qualifier, filter, params, ctes)
            } else {
                execute_function(engine, table, name, args, params)
            }
        }
        other => filter_table_rows(engine, table, qualifier, other, params, ctes),
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
