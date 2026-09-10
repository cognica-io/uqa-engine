//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mixed SQL WHERE evaluation for boolean filters and row-emitting search functions.

use super::{CteScope, SQLError, SQLParam, ScalarExpr, ScoredEntry, SourceContext};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::DocId;

struct DocumentFilterInput {
    schema: crate::RowSchema,
    document_id: uqa_sql::ast::InternalColumnRef,
    doc_ids: Vec<DocId>,
    documents: Vec<uqa_storage::document_store::Document>,
}

fn filter_documents<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    input: DocumentFilterInput,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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
            values.push(uqa_sql::semantics::doc_id_value(doc_id)?);
            Ok(crate::PhysicalRow::from_values(values))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let rows = crate::query::relational::output::execute_filter_physical_rows(
        context.relational,
        schema,
        rows,
        filter.clone(),
        params,
        ctes,
    )?;
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

fn filter_table_rows<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    table: &str,
    qualifier: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let doc_ids = context.documents.document_ids(table)?;
    // When the predicate reads a known column set, evaluate it against
    // a per-row field projection fetched in one storage scan instead
    // of materialising every document.
    let mut columns = std::collections::BTreeSet::new();
    let collected_columns = filter.collect_columns(&mut columns);
    let references_tableoid = columns.contains(uqa_sql::semantics::TABLE_OID_COLUMN);
    if collected_columns && !references_tableoid {
        let names: Vec<String> = columns.into_iter().collect();
        let documents = if names.is_empty() {
            // A constant predicate needs only the candidate document ids. Some persistent stores represent a zero-column projection as an empty result map, so asking storage for no fields incorrectly makes every listed row look missing.
            vec![uqa_storage::document_store::Document::new(); doc_ids.len()]
        } else {
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let field_values = context.documents.document_fields(table, &doc_ids, &refs)?;
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
            documents
        };
        let (schema, document_id) = table_filter_schema(context, table, qualifier, names)?;
        return filter_documents(
            context,
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
        let mut document = context.documents.document(table, doc_id)?.ok_or_else(|| {
            SQLError::Internal(format!(
                "WHERE scan: document {doc_id} listed by table `{table}` disappeared during the statement"
            ))
        })?;
        if references_tableoid {
            document.insert(
                uqa_sql::semantics::TABLE_OID_COLUMN.into(),
                uqa_core::Value::Int(crate::catalog::projection::table_relation_oid(
                    &context.catalog,
                    table,
                )?),
            );
        }
        documents.push(document);
    }
    let mut columns = context.text_indexes.column_names(table).map_err(|error| {
        SQLError::Internal(format!("read table columns for `{table}`: {error}"))
    })?;
    if references_tableoid
        && !columns
            .iter()
            .any(|column| column == uqa_sql::semantics::TABLE_OID_COLUMN)
    {
        columns.push(uqa_sql::semantics::TABLE_OID_COLUMN.into());
    }
    let (schema, document_id) = table_filter_schema(context, table, qualifier, columns)?;
    filter_documents(
        context,
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

fn table_filter_schema<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    table: &str,
    qualifier: &str,
    columns: Vec<String>,
) -> Result<(crate::RowSchema, uqa_sql::ast::InternalColumnRef), SQLError> {
    let definitions = context
        .documents
        .column_definitions(table)
        .map_err(|error| SQLError::Internal(format!("read table schema for `{table}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let types = columns
        .iter()
        .map(|column| {
            if column == uqa_sql::semantics::TABLE_OID_COLUMN {
                return Some(uqa_sql::ast::ColumnType::Oid);
            }
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    let identities = columns
        .iter()
        .map(|column| crate::ColumnIdentity::qualified(qualifier, column))
        .collect::<Vec<_>>();
    let document_id = uqa_sql::ast::InternalRelationId::allocate().column(0);
    let schema = crate::RowSchema::with_identities(columns, identities, types);
    let schema = crate::RowSchema::append_internal_typed(
        &schema,
        &[(document_id, Some(uqa_sql::ast::ColumnType::BigInteger))],
    );
    Ok((schema, document_id))
}

pub fn execute_mixed_where<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    table: &str,
    signal_table: &str,
    qualifier: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let mut rows = execute_mixed_where_expr(
        context,
        table,
        signal_table,
        qualifier,
        filter,
        params,
        ctes,
    )?;
    rows.sort_by_key(|e| e.doc_id);
    Ok(rows)
}

/// Resolve a WHERE clause to the matching doc ids with the same
/// machinery single-table SELECT uses: the operator-tree pipeline
/// (value indexes, posting lists) first, then registered row
/// functions, then the evaluated scan. UPDATE / DELETE call this so
/// point writes stop paying a full table materialisation.
pub fn collect_where_doc_ids<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    table: &str,
    qualifier: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Vec<DocId>, SQLError> {
    let references_tableoid = expression_references_tableoid(filter);
    let optimized = if references_tableoid {
        None
    } else {
        context
            .relation_retrieval
            .optimized(table, Some(filter), params)?
    };
    let scored = if is_jsonpath_fts_match_filter(filter) {
        execute_mixed_where_expr(context, table, table, qualifier, filter, params, ctes)?
    } else if let Some(entries) = optimized {
        entries
    } else {
        match filter {
            ScalarExpr::Func { name, args, .. } if uqa_sql::registry::is_registered(name) => {
                context
                    .relation_retrieval
                    .function(table, table, name, args, params, None)?
            }
            other => {
                execute_mixed_where_expr(context, table, table, qualifier, other, params, ctes)?
            }
        }
    };
    Ok(scored.into_iter().map(|entry| entry.doc_id).collect())
}

fn expression_references_tableoid(expression: &ScalarExpr) -> bool {
    let mut columns = BTreeSet::new();
    expression.collect_columns(&mut columns)
        && columns.contains(uqa_sql::semantics::TABLE_OID_COLUMN)
}

fn is_jsonpath_fts_match_filter(filter: &ScalarExpr) -> bool {
    match filter {
        ScalarExpr::Func { name, args, .. } => is_jsonpath_fts_match(name, args),
        _ => false,
    }
}

fn execute_mixed_where_expr<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    table: &str,
    signal_table: &str,
    qualifier: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    match filter {
        ScalarExpr::And(parts) => {
            let mut iter = parts.iter();
            let Some(first) = iter.next() else {
                return all_table_rows(context, table);
            };
            let mut out = execute_mixed_where_expr(
                context,
                table,
                signal_table,
                qualifier,
                first,
                params,
                ctes,
            )?;
            for part in iter {
                let rhs = execute_mixed_where_expr(
                    context,
                    table,
                    signal_table,
                    qualifier,
                    part,
                    params,
                    ctes,
                )?;
                out = intersect_scored(out, rhs);
            }
            Ok(out)
        }
        ScalarExpr::Or(parts) => {
            let mut out = Vec::new();
            for part in parts {
                out = union_scored(
                    out,
                    execute_mixed_where_expr(
                        context,
                        table,
                        signal_table,
                        qualifier,
                        part,
                        params,
                        ctes,
                    )?,
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
            context,
            table,
            execute_mixed_where_expr(context, table, signal_table, qualifier, inner, params, ctes)?,
        ),
        ScalarExpr::Func { name, args, .. } if uqa_sql::registry::is_registered(name) => {
            if is_jsonpath_fts_match(name, args) {
                filter_table_rows(context, table, qualifier, filter, params, ctes)
            } else {
                context
                    .relation_retrieval
                    .function(table, signal_table, name, args, params, None)
            }
        }
        other => filter_table_rows(context, table, qualifier, other, params, ctes),
    }
}

fn is_jsonpath_fts_match(name: &str, args: &[ScalarExpr]) -> bool {
    name.eq_ignore_ascii_case("fts_match")
        && matches!(
            args.get(1),
            Some(ScalarExpr::Literal(uqa_core::Value::Str(path))) if path.trim_start().starts_with('$')
        )
}

pub use uqa_sql::semantics::expr_is_null_free;

fn all_table_rows<S: Clone + 'static>(
    context: &SourceContext<'_, S>,
    table: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    Ok(context
        .documents
        .document_ids(table)?
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

fn complement_scored<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    table: &str,
    rows: Vec<ScoredEntry>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let excluded: BTreeSet<DocId> = rows.into_iter().map(|e| e.doc_id).collect();
    Ok(context
        .documents
        .document_ids(table)?
        .into_iter()
        .filter(|doc_id| !excluded.contains(doc_id))
        .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
        .collect())
}
