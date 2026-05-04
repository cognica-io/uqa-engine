//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Engine::sql` driver: parse SQL via `uqa_sql::compile`, lower each
//! statement onto the engine's mutation / search APIs, and roll the
//! result rows into a [`SqlResult`].
//!
//! Phase 5 covers the quickstart slice: `CREATE TABLE` (with `VECTOR(N)`
//! columns), `CREATE INDEX ... USING gin (...)` (recorded as an FTS
//! field), `INSERT ... VALUES`, and `SELECT` with `text_match`,
//! `knn_match`, and `fuse_log_odds` calls in `WHERE`. Statements outside
//! this surface return [`uqa_sql::SqlError::Unsupported`] cleanly.

#![allow(
    clippy::useless_format,
    clippy::manual_let_else,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::items_after_statements
)]

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{
    ColumnDef as SqlColumnDef, ColumnType, CreateIndex, CreateTable, Expr, InsertStmt, Projection,
    SelectStmt, Statement,
};
use uqa_sql::expr::{eval, value_to_vector, EvalContext};
use uqa_sql::registry::{lookup, FunctionKind};
use uqa_sql::{compile, ResultRow, SqlError, SqlParam, SqlResult};
use uqa_storage::document_store::Document;

use crate::{Engine, HybridSearchParams, ScoredEntry};

const SCORE_COLUMN: &str = "_score";

pub fn execute(engine: &Engine, sql: &str, params: &[SqlParam]) -> Result<SqlResult, SqlError> {
    let stmts = compile(sql)?;
    if stmts.is_empty() {
        return Ok(SqlResult::empty());
    }
    let mut last = SqlResult::empty();
    for stmt in stmts {
        last = run_stmt(engine, stmt, params)?;
    }
    Ok(last)
}

fn run_stmt(engine: &Engine, stmt: Statement, params: &[SqlParam]) -> Result<SqlResult, SqlError> {
    match stmt {
        Statement::CreateTable(c) => run_create_table(engine, c),
        Statement::CreateIndex(c) => run_create_index(engine, c),
        Statement::Insert(i) => run_insert(engine, i, params),
        Statement::Select(s) => run_select(engine, s, params),
    }
}

// -------------------------------------------------------------------------
// DDL
// -------------------------------------------------------------------------

fn run_create_table(engine: &Engine, c: CreateTable) -> Result<SqlResult, SqlError> {
    let mut fts_fields = Vec::new();
    let mut vector_fields: Vec<(String, u32)> = Vec::new();
    for col in &c.columns {
        match &col.ty {
            ColumnType::Text => fts_fields.push(col.name.clone()),
            ColumnType::Vector(dim) => vector_fields.push((col.name.clone(), *dim)),
            _ => {}
        }
    }
    engine.create_default_table(c.name.clone(), fts_fields);
    for (field, dim) in vector_fields {
        engine.create_vector_field(&c.name, field, dim);
    }
    let _ = column_names(&c.columns); // sanity, used by future EXPLAIN
    Ok(SqlResult::empty())
}

fn column_names(cols: &[SqlColumnDef]) -> Vec<String> {
    cols.iter().map(|c| c.name.clone()).collect()
}

fn run_create_index(_engine: &Engine, _c: CreateIndex) -> Result<SqlResult, SqlError> {
    // FTS fields are derived from TEXT columns at CREATE TABLE time, so
    // CREATE INDEX is informational only in Phase 5. We accept any
    // access method (gin, btree, ivf, rtree) and treat it as a no-op.
    Ok(SqlResult::empty())
}

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------

fn run_insert(
    engine: &Engine,
    stmt: InsertStmt,
    params: &[SqlParam],
) -> Result<SqlResult, SqlError> {
    if stmt.columns.is_empty() {
        return Err(SqlError::Unsupported(
            "INSERT without explicit column list".into(),
        ));
    }
    let id_index = stmt
        .columns
        .iter()
        .position(|c| c == "id")
        .ok_or_else(|| SqlError::Unsupported("INSERT requires an `id` column".into()))?;

    let mut affected = 0u64;
    let ctx = EvalContext::new(None, params);
    for row in &stmt.rows {
        if row.len() != stmt.columns.len() {
            return Err(SqlError::Internal(format!(
                "row width {} != column count {}",
                row.len(),
                stmt.columns.len()
            )));
        }
        let mut document = Document::new();
        let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut doc_id: Option<u64> = None;
        for (i, col) in stmt.columns.iter().enumerate() {
            let v = eval(&row[i], &ctx)?;
            if i == id_index {
                doc_id = match &v {
                    Value::Int(n) if *n >= 0 => Some(*n as u64),
                    other => {
                        return Err(SqlError::TypeMismatch(format!(
                            "id must be a non-negative integer, got {other:?}"
                        )));
                    }
                };
            }
            // Heuristic: rows whose value compiles to a list go to the
            // vector index; the document store keeps the raw value too
            // so the projection can read it back later.
            if let Ok(vec) = value_to_vector(&v) {
                vectors.insert(col.clone(), vec);
            }
            document.insert(col.clone(), v);
        }
        let doc_id = doc_id.ok_or_else(|| SqlError::Internal("INSERT missing id value".into()))?;
        engine.add_document_with_vectors(
            &stmt.table,
            doc_id,
            document,
            vectors_to_field_map(vectors),
        );
        affected += 1;
    }
    Ok(SqlResult::from_affected(affected))
}

fn vectors_to_field_map(
    vectors: BTreeMap<String, Vec<f32>>,
) -> BTreeMap<uqa_core::FieldName, Vec<f32>> {
    vectors
}

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

fn run_select(
    engine: &Engine,
    stmt: SelectStmt,
    params: &[SqlParam],
) -> Result<SqlResult, SqlError> {
    let table = stmt
        .from
        .as_deref()
        .ok_or_else(|| SqlError::Unsupported("SELECT without FROM".into()))?;

    // The WHERE clause picks the search strategy. Phase 5 handles a
    // single function call (text_match / knn_match / fuse_log_odds) at
    // the top level; richer Boolean / nested forms land in Phase 6.
    let scored = match stmt.r#where.as_ref() {
        None => engine
            .table_doc_ids(table)
            .into_iter()
            .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
            .collect::<Vec<_>>(),
        Some(Expr::Func { name, args }) => execute_function(engine, table, name, args, params)?,
        Some(other) => {
            return Err(SqlError::Unsupported(format!(
                "WHERE form not supported in Phase 5: {other:?}"
            )));
        }
    };

    let scored = apply_order_limit(scored, &stmt);
    let columns = projection_columns(&stmt.projections);
    let rows = build_rows(engine, table, &scored, &stmt.projections, params)?;
    Ok(SqlResult::from_rows(columns, rows))
}

fn execute_function(
    engine: &Engine,
    table: &str,
    name: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    let kind = lookup(name).ok_or_else(|| SqlError::UnknownFunction(name.to_string()))?;
    match kind {
        FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
            run_text_match(engine, table, args, params)
        }
        FunctionKind::KnnMatch => run_knn_match(engine, table, args, params),
        FunctionKind::FuseLogOdds => run_fuse_log_odds(engine, table, args, params),
    }
}

fn run_text_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() != 2 {
        return Err(SqlError::BadArity {
            name: "text_match".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "text_match.field")?;
    let ctx = EvalContext::new(None, params);
    let query_value = eval(&args[1], &ctx)?;
    let query = match query_value {
        Value::Str(s) => s,
        other => {
            return Err(SqlError::TypeMismatch(format!(
                "text_match query must be a string, got {other:?}"
            )));
        }
    };
    let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
    Ok(engine.search(table, &field, &query, &mode, usize::MAX))
}

fn run_knn_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() != 3 {
        return Err(SqlError::BadArity {
            name: "knn_match".into(),
            expected: "3".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "knn_match.field")?;
    let ctx = EvalContext::new(None, params);
    let vec_value = eval(&args[1], &ctx)?;
    let query_vector = value_to_vector(&vec_value)?;
    let k = expect_usize(&args[2], "knn_match.k", &ctx)?;
    Ok(engine.knn_search(table, &field, query_vector, k))
}

fn run_fuse_log_odds(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SqlParam],
) -> Result<Vec<ScoredEntry>, SqlError> {
    if args.len() < 2 {
        return Err(SqlError::BadArity {
            name: "fuse_log_odds".into(),
            expected: ">=2".into(),
            actual: args.len(),
        });
    }
    // Phase 5 supports the canonical hybrid call shape:
    // `fuse_log_odds(text_match(field, q), knn_match(vec_field, $1, k))`.
    let mut text_field: Option<String> = None;
    let mut text_query: Option<String> = None;
    let mut vector_field: Option<String> = None;
    let mut query_vector: Option<Vec<f32>> = None;
    let mut knn_pool: usize = 10;
    let ctx = EvalContext::new(None, params);
    for arg in args {
        match arg {
            Expr::Func { name, args: inner } => {
                let kind = lookup(name).ok_or_else(|| SqlError::UnknownFunction(name.clone()))?;
                match kind {
                    FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
                        if inner.len() != 2 {
                            return Err(SqlError::BadArity {
                                name: name.clone(),
                                expected: "2".into(),
                                actual: inner.len(),
                            });
                        }
                        text_field = Some(expect_column_name(&inner[0], "text_match.field")?);
                        let q = eval(&inner[1], &ctx)?;
                        text_query = match q {
                            Value::Str(s) => Some(s),
                            other => {
                                return Err(SqlError::TypeMismatch(format!(
                                    "text_match query must be string, got {other:?}"
                                )));
                            }
                        };
                    }
                    FunctionKind::KnnMatch => {
                        if inner.len() != 3 {
                            return Err(SqlError::BadArity {
                                name: name.clone(),
                                expected: "3".into(),
                                actual: inner.len(),
                            });
                        }
                        vector_field = Some(expect_column_name(&inner[0], "knn_match.field")?);
                        query_vector = Some(value_to_vector(&eval(&inner[1], &ctx)?)?);
                        knn_pool = expect_usize(&inner[2], "knn_match.k", &ctx)?;
                    }
                    FunctionKind::FuseLogOdds => {
                        return Err(SqlError::Unsupported(
                            "nested fuse_log_odds is not supported".into(),
                        ));
                    }
                }
            }
            other => {
                return Err(SqlError::Unsupported(format!(
                    "fuse_log_odds argument must be a function call, got {other:?}"
                )));
            }
        }
    }
    let text_field = text_field
        .ok_or_else(|| SqlError::Unsupported("fuse_log_odds requires a text_match arm".into()))?;
    let text_query = text_query.unwrap_or_default();
    let vector_field = vector_field
        .ok_or_else(|| SqlError::Unsupported("fuse_log_odds requires a knn_match arm".into()))?;
    let query_vector =
        query_vector.ok_or_else(|| SqlError::Internal("missing knn_match vector".into()))?;

    Ok(engine.hybrid_search(&HybridSearchParams {
        table,
        text_field: &text_field,
        text_query: &text_query,
        vector_field: &vector_field,
        query_vector,
        knn_pool,
        alpha: 0.5,
        top_k: usize::MAX,
    }))
}

fn expect_column_name(expr: &Expr, label: &str) -> Result<String, SqlError> {
    match expr {
        Expr::Column(name) => Ok(name.clone()),
        other => Err(SqlError::TypeMismatch(format!(
            "{label} must be a column reference, got {other:?}"
        ))),
    }
}

fn expect_usize(expr: &Expr, label: &str, ctx: &EvalContext<'_>) -> Result<usize, SqlError> {
    let v = eval(expr, ctx)?;
    match v {
        Value::Int(n) if n >= 0 => Ok(n as usize),
        Value::Int(_) => Err(SqlError::TypeMismatch(format!("{label} must be >= 0"))),
        other => Err(SqlError::TypeMismatch(format!(
            "{label} must be an integer, got {other:?}"
        ))),
    }
}

// -------------------------------------------------------------------------
// Output assembly
// -------------------------------------------------------------------------

fn apply_order_limit(mut entries: Vec<ScoredEntry>, stmt: &SelectStmt) -> Vec<ScoredEntry> {
    if !stmt.order_by.is_empty() {
        // Phase 5 only honours `ORDER BY <score-alias|_score>`. Any
        // expression resolves to the entry's score in our limited
        // surface; richer ordering lands in Phase 6 with the proper
        // expression evaluator.
        let descending = stmt.order_by.iter().any(|o| o.descending);
        entries.sort_by(|a, b| {
            let cmp = a
                .score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal);
            if descending { cmp.reverse() } else { cmp }.then_with(|| a.doc_id.cmp(&b.doc_id))
        });
    }
    if let Some(offset) = stmt.offset {
        let off = offset as usize;
        if off >= entries.len() {
            entries.clear();
        } else {
            entries.drain(0..off);
        }
    }
    if let Some(limit) = stmt.limit {
        entries.truncate(limit as usize);
    }
    entries
}

fn projection_columns(projections: &[Projection]) -> Vec<String> {
    projections
        .iter()
        .map(|p| match (&p.alias, &p.expr) {
            (Some(a), _) => a.clone(),
            (None, Expr::Column(c)) => c.clone(),
            (None, Expr::Star) => "*".to_string(),
            (None, _) => "expr".to_string(),
        })
        .collect()
}

fn build_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    projections: &[Projection],
    params: &[SqlParam],
) -> Result<Vec<ResultRow>, SqlError> {
    let mut rows = Vec::with_capacity(scored.len());
    for entry in scored {
        let mut document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        document.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
        let row = build_projection_row(&document, projections, params)?;
        rows.push(row);
    }
    Ok(rows)
}

fn build_projection_row(
    document: &Document,
    projections: &[Projection],
    params: &[SqlParam],
) -> Result<ResultRow, SqlError> {
    let ctx = EvalContext::new(Some(document), params);
    let mut row = ResultRow::new();
    for proj in projections {
        let label = match (&proj.alias, &proj.expr) {
            (Some(a), _) => a.clone(),
            (None, Expr::Column(c)) => c.clone(),
            (None, Expr::Star) => "*".into(),
            (None, _) => "expr".into(),
        };
        if let Expr::Star = proj.expr {
            for (k, v) in document {
                if k.as_str() == SCORE_COLUMN {
                    continue;
                }
                row.insert(k.clone(), v.clone());
            }
            continue;
        }
        let value = eval(&proj.expr, &ctx)?;
        row.insert(label, value);
    }
    Ok(row)
}

impl Engine {
    /// Run an arbitrary SQL statement against the engine. Phase 5
    /// supports the quickstart slice; statements outside the supported
    /// grammar return a structured `Unsupported` error.
    pub fn sql(&self, query: &str, params: &[SqlParam]) -> Result<SqlResult, SqlError> {
        execute(self, query, params)
    }

    /// All doc ids on a table, used by the SELECT path when there is no
    /// WHERE clause.
    pub fn table_doc_ids(&self, table: &str) -> Vec<uqa_core::DocId> {
        let Some(t) = self.tables.read().get(table).cloned() else {
            return Vec::new();
        };
        let ids = t.document_store.read().doc_ids();
        ids
    }
}
