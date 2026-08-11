//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `QueryBuilder` assembles a `SELECT` statement and runs it via
//! [`uqa_engine::Engine::sql`]. Infallible methods return the builder by
//! value, while helpers that validate SQL literals or retrieval options
//! return `Result<Self, SQLError>` and compose with `?`. The builder is
//! transport-only; all real work happens in the engine's SQL pipeline.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};
use uqa_sql::SQLError;

use std::fmt::{self, Write as _};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{ArrowError, DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::errors::ParquetError;

mod literals;
mod output;
mod validation;

use literals::{quote_str, render_value};
pub use output::QueryBuilderError;
#[cfg(test)]
use output::{infer_arrow_type, sql_result_to_record_batch};
use validation::{
    render_vector, validate_field_name, validate_fusion_alpha, validate_probability_threshold,
    validate_retrieval_signals, validate_stage_count, validate_stage_cutoffs,
    validate_vector_query,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Asc,
    Desc,
}

#[derive(Clone)]
pub struct QueryBuilder<'a> {
    engine: &'a Engine,
    table: String,
    projections: Vec<String>,
    filters: Vec<String>,
    group_by: Vec<String>,
    order_by: Vec<(String, Order)>,
    limit: Option<u64>,
    offset: Option<u64>,
}

fn render_exact_fusion(
    function_name: &str,
    signals: &[&str],
    base_rate: Option<f64>,
) -> Result<String, SQLError> {
    validate_retrieval_signals(function_name, signals, 2)?;
    if let Some(base_rate) = base_rate {
        if !base_rate.is_finite() || base_rate <= 0.0 || base_rate >= 1.0 {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name} base_rate must be finite and in (0, 1), got {base_rate}"
            )));
        }
    }
    let mut arguments = signals.join(", ");
    if let Some(base_rate) = base_rate {
        write!(arguments, ", base_rate => {base_rate}").expect("writing to String cannot fail");
    }
    Ok(format!("{function_name}({arguments})"))
}

impl<'a> QueryBuilder<'a> {
    pub fn new(engine: &'a Engine, table: impl Into<String>) -> Self {
        Self {
            engine,
            table: table.into(),
            projections: Vec::new(),
            filters: Vec::new(),
            group_by: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    /// Replace the SELECT projection list. Pass column names verbatim;
    /// pass `*` for star projection.
    pub fn select_columns<S: AsRef<str>>(mut self, columns: &[S]) -> Self {
        self.projections = columns.iter().map(|s| s.as_ref().to_string()).collect();
        self
    }

    /// Add a single projection expression. The argument is inserted
    /// into the SQL verbatim, so callers can pass aliased expressions
    /// like `count(*) AS n`.
    pub fn select(mut self, expr: impl Into<String>) -> Self {
        self.projections.push(expr.into());
        self
    }

    /// Add an arbitrary boolean expression to the `WHERE` clause. All
    /// added filters combine with `AND`.
    pub fn r#where(mut self, predicate: impl Into<String>) -> Self {
        self.filters.push(predicate.into());
        self
    }

    /// Convenience: `<column> = <value>` filter.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` cannot be represented losslessly as a
    /// SQL literal, such as a map or a non-finite float.
    pub fn where_eq(self, column: &str, value: &Value) -> Result<Self, SQLError> {
        Ok(self.r#where(format!("{column} = {}", render_value(value)?)))
    }

    /// Convenience: `<column> > <value>` filter.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` cannot be represented losslessly as a
    /// SQL literal, such as a map or a non-finite float.
    pub fn where_gt(self, column: &str, value: &Value) -> Result<Self, SQLError> {
        Ok(self.r#where(format!("{column} > {}", render_value(value)?)))
    }

    /// Convenience: `<column> >= <value>` filter.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` cannot be represented losslessly as a
    /// SQL literal, such as a map or a non-finite float.
    pub fn where_gte(self, column: &str, value: &Value) -> Result<Self, SQLError> {
        Ok(self.r#where(format!("{column} >= {}", render_value(value)?)))
    }

    /// Convenience: `<column> < <value>` filter.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` cannot be represented losslessly as a
    /// SQL literal, such as a map or a non-finite float.
    pub fn where_lt(self, column: &str, value: &Value) -> Result<Self, SQLError> {
        Ok(self.r#where(format!("{column} < {}", render_value(value)?)))
    }

    /// Convenience: `<column> <= <value>` filter.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` cannot be represented losslessly as a
    /// SQL literal, such as a map or a non-finite float.
    pub fn where_lte(self, column: &str, value: &Value) -> Result<Self, SQLError> {
        Ok(self.r#where(format!("{column} <= {}", render_value(value)?)))
    }

    /// Add `text_match(field, '<query>')` to WHERE.
    pub fn text_match(self, field: &str, query: &str) -> Self {
        self.r#where(format!("text_match({field}, {})", quote_str(query)))
    }

    /// Add `knn_match(field, ARRAY[v1, v2, ...], k)` to `WHERE`.
    ///
    /// # Errors
    ///
    /// Returns an error when `field` is empty, `vector` is empty or contains
    /// a non-finite component, or `k` is zero or does not fit in SQL `BIGINT`.
    pub fn knn_match(self, field: &str, vector: &[f32], k: usize) -> Result<Self, SQLError> {
        validate_vector_query("knn_match", field, vector, k)?;
        let arr = vector
            .iter()
            .map(f32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(self.r#where(format!("knn_match({field}, ARRAY[{arr}], {k})")))
    }

    /// Add `multi_field_match(field_1, '<q1>', field_2, '<q2>', ...)` to
    /// `WHERE`.
    ///
    /// # Errors
    ///
    /// Returns an error unless at least two field/query pairs are supplied,
    /// or when any field name is empty.
    pub fn multi_field_match(self, fields_and_queries: &[(&str, &str)]) -> Result<Self, SQLError> {
        if fields_and_queries.len() < 2 {
            return Err(SQLError::BadArity {
                name: "multi_field_match".into(),
                expected: ">=2 field/query pairs".into(),
                actual: fields_and_queries.len(),
            });
        }
        if fields_and_queries
            .iter()
            .any(|(field, _)| field.trim().is_empty())
        {
            return Err(SQLError::TypeMismatch(
                "multi_field_match field names cannot be empty".into(),
            ));
        }
        let mut parts = Vec::with_capacity(fields_and_queries.len() * 2);
        for (field, query) in fields_and_queries {
            parts.push((*field).to_string());
            parts.push(quote_str(query));
        }
        Ok(self.r#where(format!("multi_field_match({})", parts.join(", "))))
    }

    /// Add `staged_retrieval(f1, '<q1>', k1, f2, '<q2>', k2, ...)` to
    /// `WHERE`.
    ///
    /// # Errors
    ///
    /// Returns an error when there are no stages, a field name is empty, or
    /// a stage cutoff is zero or does not fit in SQL `BIGINT`.
    pub fn staged_retrieval(self, stages: &[(&str, &str, usize)]) -> Result<Self, SQLError> {
        validate_stage_count(stages.len())?;
        validate_stage_cutoffs(stages.iter().map(|(_, _, top_k)| *top_k))?;
        if stages.iter().any(|(field, _, _)| field.trim().is_empty()) {
            return Err(SQLError::TypeMismatch(
                "staged_retrieval field names cannot be empty".into(),
            ));
        }
        let mut parts = Vec::with_capacity(stages.len() * 3);
        for (field, query, top_k) in stages {
            parts.push((*field).to_string());
            parts.push(quote_str(query));
            parts.push(top_k.to_string());
        }
        Ok(self.r#where(format!("staged_retrieval({})", parts.join(", "))))
    }

    /// Add `graph_pagerank('<graph>')` to WHERE.
    pub fn graph_pagerank(self, graph_name: &str) -> Self {
        self.r#where(format!("graph_pagerank({})", quote_str(graph_name)))
    }

    /// Add `graph_traverse('<graph>', start, label?, max_hops)` to WHERE.
    pub fn graph_traverse(
        self,
        graph_name: &str,
        start_vertex: u64,
        label: Option<&str>,
        max_hops: u32,
    ) -> Self {
        let label_arg = match label {
            Some(l) => quote_str(l),
            None => "NULL".to_string(),
        };
        self.r#where(format!(
            "graph_traverse({}, {start_vertex}, {label_arg}, {max_hops})",
            quote_str(graph_name)
        ))
    }

    /// Add `graph_neighbors('<graph>', vertex, label?, '<direction>')` to WHERE.
    pub fn graph_neighbors(
        self,
        graph_name: &str,
        vertex_id: u64,
        label: Option<&str>,
        direction: &str,
    ) -> Self {
        let label_arg = match label {
            Some(l) => quote_str(l),
            None => "NULL".to_string(),
        };
        self.r#where(format!(
            "graph_neighbors({}, {vertex_id}, {label_arg}, {})",
            quote_str(graph_name),
            quote_str(direction)
        ))
    }

    /// Add `deep_predict('<model>')` to WHERE.
    pub fn deep_predict(self, model_name: &str) -> Self {
        self.r#where(format!("deep_predict({})", quote_str(model_name)))
    }

    pub fn order_by(mut self, column: impl Into<String>, order: Order) -> Self {
        self.order_by.push((column.into(), order));
        self
    }

    pub fn order_by_asc(self, column: impl Into<String>) -> Self {
        self.order_by(column, Order::Asc)
    }

    pub fn order_by_desc(self, column: impl Into<String>) -> Self {
        self.order_by(column, Order::Desc)
    }

    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// Render the SQL string the builder would execute. Useful for
    /// logging, test introspection, or `EXPLAIN`-style diagnostics.
    #[allow(clippy::items_after_statements)]
    pub fn to_sql(&self) -> String {
        let projection = if self.projections.is_empty() {
            "*".to_string()
        } else {
            self.projections.join(", ")
        };
        let mut sql = format!("SELECT {projection} FROM {}", self.table);
        if !self.filters.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.filters.join(" AND "));
        }
        if !self.group_by.is_empty() {
            sql.push_str(" GROUP BY ");
            sql.push_str(&self.group_by.join(", "));
        }
        if !self.order_by.is_empty() {
            sql.push_str(" ORDER BY ");
            let pieces: Vec<String> = self
                .order_by
                .iter()
                .map(|(col, ord)| match ord {
                    Order::Asc => format!("{col} ASC"),
                    Order::Desc => format!("{col} DESC"),
                })
                .collect();
            sql.push_str(&pieces.join(", "));
        }
        if let Some(limit) = self.limit {
            let _ = write!(sql, " LIMIT {limit}");
        }
        if let Some(offset) = self.offset {
            let _ = write!(sql, " OFFSET {offset}");
        }
        sql
    }

    pub fn execute(&self) -> Result<SQLResult, SQLError> {
        self.engine.sql(&self.to_sql(), &[])
    }

    // -----------------------------------------------------------------
    // Fluent full-text and retrieval helpers.
    // -----------------------------------------------------------------

    /// Add a bare term filter (`text_match(field, 'term')`). When
    /// `field` is `None`, the SQL function falls back to all-field
    /// search via the engine's analyzer registry.
    pub fn term(self, term: &str, field: Option<&str>) -> Self {
        match field {
            Some(f) => self.text_match(f, term),
            None => self.r#where(format!("fts_match('_all', {})", quote_str(term))),
        }
    }

    /// Combine two builders' filter lists with `AND`. The resulting
    /// builder keeps the receiver's projection / order / limit /
    /// offset state and absorbs `other`'s filters.
    pub fn and(mut self, other: &QueryBuilder<'a>) -> Self {
        for f in &other.filters {
            self.filters.push(f.clone());
        }
        self
    }

    /// Wrap the builder's filter list in an `OR` group with `other`'s
    /// filters: `((self_filters) OR (other_filters))`.
    pub fn or(mut self, other: &QueryBuilder<'a>) -> Self {
        let lhs = self.filters.join(" AND ");
        let rhs = other.filters.join(" AND ");
        let merged = match (lhs.is_empty(), rhs.is_empty()) {
            (true, true) => String::new(),
            (false, true) => lhs,
            (true, false) => rhs,
            (false, false) => format!("({lhs}) OR ({rhs})"),
        };
        self.filters = if merged.is_empty() {
            Vec::new()
        } else {
            vec![merged]
        };
        self
    }

    /// Negate the current filter list. Renders to `NOT (a AND b ...)`.
    #[allow(clippy::should_implement_trait)]
    pub fn not(mut self) -> Self {
        if self.filters.is_empty() {
            return self;
        }
        let combined = self.filters.join(" AND ");
        self.filters = vec![format!("NOT ({combined})")];
        self
    }

    /// Add a nearest-neighbor vector retrieval predicate. This compatibility
    /// name delegates to the registered `knn_match` SQL function.
    ///
    /// # Errors
    ///
    /// Returns an error under the same conditions as [`Self::knn_match`].
    pub fn vector(self, query: &[f32], k: usize, field: &str) -> Result<Self, SQLError> {
        self.knn_match(field, query, k)
    }

    /// Promote the projection list to a single aggregate over `field`,
    /// e.g. `SELECT count(field) FROM ...`. Resets any previous
    /// projections.
    pub fn aggregate(mut self, field: &str, agg: &str) -> Self {
        self.projections.clear();
        self.group_by.clear();
        self.projections.push(format!("{agg}({field})"));
        self
    }

    /// Replace projections with `field, count(*)` and add a
    /// `GROUP BY field` ordering hint. The engine's GROUP BY pipeline
    /// picks up the projection.
    pub fn facet(mut self, field: &str) -> Self {
        self.projections.clear();
        self.projections.push(field.to_string());
        self.projections.push("count(*) AS _facet_count".into());
        self.group_by = vec![field.to_string()];
        self.order_by_desc("_facet_count")
    }

    /// Add `score_bm25(field, 'query')` to the projection list.
    pub fn score_bm25(mut self, query: &str, field: Option<&str>) -> Self {
        let proj = match field {
            Some(f) => format!("score_bm25({f}, {})", quote_str(query)),
            None => format!("score_bm25({})", quote_str(query)),
        };
        self.projections.push(proj);
        self
    }

    /// Add `score_bayesian_bm25(field, 'query')` to the projection
    /// list.
    pub fn score_bayesian_bm25(mut self, query: &str, field: Option<&str>) -> Self {
        let proj = match field {
            Some(f) => format!("score_bayesian_bm25({f}, {})", quote_str(query)),
            None => format!("score_bayesian_bm25({})", quote_str(query)),
        };
        self.projections.push(proj);
        self
    }

    /// Add exact signed-evidence log-odds fusion. This is an alias for
    /// [`Self::fuse_bayesian_evidence`]: each signal must emit a prior-free
    /// evidence probability, and the optional corpus prior enters once.
    ///
    /// # Errors
    ///
    /// Returns an error unless at least two non-empty signal expressions are
    /// supplied, or when `base_rate` is not finite and strictly between zero
    /// and one.
    pub fn fuse_log_odds(self, signals: &[&str], base_rate: Option<f64>) -> Result<Self, SQLError> {
        let predicate = render_exact_fusion("fuse_log_odds", signals, base_rate)?;
        Ok(self.r#where(predicate))
    }

    /// Add a gated, confidence-scaled positive-evidence retrieval pool.
    /// This operator is a ranking heuristic and does not claim exact Bayesian
    /// posterior semantics.
    ///
    /// # Errors
    ///
    /// Returns an error unless at least two non-empty signal expressions are
    /// supplied, or when `alpha` is non-finite or outside `[0, 1]`.
    pub fn pool_positive_evidence(self, signals: &[&str], alpha: f64) -> Result<Self, SQLError> {
        validate_retrieval_signals("pool_positive_evidence", signals, 2)?;
        validate_fusion_alpha("pool_positive_evidence", alpha)?;
        let inner = signals.join(", ");
        Ok(self.r#where(format!("pool_positive_evidence({inner}, {alpha})")))
    }

    /// Add exact signed-evidence Bayesian fusion. Each signal must emit a
    /// prior-free evidence probability; the optional corpus prior enters once.
    ///
    /// # Errors
    ///
    /// Returns an error unless at least two non-empty signals are supplied, or
    /// when `base_rate` is not finite and strictly between zero and one.
    pub fn fuse_bayesian_evidence(
        self,
        signals: &[&str],
        base_rate: Option<f64>,
    ) -> Result<Self, SQLError> {
        let predicate = render_exact_fusion("fuse_bayesian_evidence", signals, base_rate)?;
        Ok(self.r#where(predicate))
    }

    /// Add a `staged_retrieval(...)` predicate from pre-rendered retrieval
    /// signals paired with their `top_k` cutoffs. The builder method retains
    /// its current name, while the generated SQL uses the registered
    /// shared-IR function.
    ///
    /// # Errors
    ///
    /// Returns an error when there are no stages, a signal expression is
    /// empty, or a cutoff is zero or does not fit in SQL `BIGINT`.
    pub fn multi_stage(self, stages: &[(&str, usize)]) -> Result<Self, SQLError> {
        validate_stage_count(stages.len())?;
        validate_stage_cutoffs(stages.iter().map(|(_, top_k)| *top_k))?;
        if stages.iter().any(|(signal, _)| signal.trim().is_empty()) {
            return Err(SQLError::TypeMismatch(
                "staged_retrieval signal expressions cannot be empty".into(),
            ));
        }
        let mut parts: Vec<String> = Vec::with_capacity(stages.len() * 2);
        for (signal, top_k) in stages {
            parts.push((*signal).to_string());
            parts.push(top_k.to_string());
        }
        Ok(self.r#where(format!("staged_retrieval({})", parts.join(", "))))
    }

    /// Add a multi-signal attention-fusion retrieval predicate. `signals`
    /// must be probability-valued expressions such as `bayesian_match(...)`
    /// or `calibrated_vector_match(...)`.
    ///
    /// # Errors
    ///
    /// Returns an error unless at least two non-empty signal expressions are
    /// supplied.
    pub fn fuse_attention(self, signals: &[&str]) -> Result<Self, SQLError> {
        validate_retrieval_signals("fuse_attention", signals, 2)?;
        Ok(self.r#where(format!("fuse_attention({})", signals.join(", "))))
    }

    /// Add a learned-fusion retrieval predicate over probability-valued
    /// signals. `alpha` controls the conjunction strength and defaults to the
    /// engine's `0.5` when omitted.
    ///
    /// # Errors
    ///
    /// Returns an error unless at least two non-empty signal expressions are
    /// supplied, or when `alpha` is non-finite or outside `[0, 1]`.
    pub fn fuse_learned(self, signals: &[&str], alpha: Option<f64>) -> Result<Self, SQLError> {
        validate_retrieval_signals("fuse_learned", signals, 2)?;
        let mut parts = signals
            .iter()
            .map(|signal| (*signal).to_string())
            .collect::<Vec<_>>();
        if let Some(alpha) = alpha {
            validate_fusion_alpha("fuse_learned", alpha)?;
            parts.push(format!("alpha => {alpha}"));
        }
        Ok(self.r#where(format!("fuse_learned({})", parts.join(", "))))
    }

    /// Add the query-pool KNN score transform to `WHERE` through its
    /// compatibility SQL name. Scores lie in `[0, 1]`, but are not described
    /// as calibrated probabilities without a held-out evaluation.
    ///
    /// # Errors
    ///
    /// Returns an error when `field` is empty, `vector` is empty or contains
    /// a non-finite component, `k` is zero or does not fit in SQL `BIGINT`, or
    /// `threshold` is non-finite or outside `[0, 1]`.
    pub fn calibrated_vector_match(
        self,
        field: &str,
        vector: &[f32],
        k: usize,
        threshold: Option<f32>,
    ) -> Result<Self, SQLError> {
        validate_vector_query("calibrated_vector_match", field, vector, k)?;
        if let Some(threshold) = threshold {
            validate_probability_threshold("calibrated_vector_match", f64::from(threshold))?;
        }
        let v = render_vector(vector);
        let predicate = match threshold {
            Some(t) => format!(
                "calibrated_vector_match({}, ARRAY[{v}], {k}, {t})",
                quote_str(field)
            ),
            None => format!(
                "calibrated_vector_match({}, ARRAY[{v}], {k})",
                quote_str(field)
            ),
        };
        Ok(self.r#where(predicate))
    }

    /// Build an `RPQ` (Regular Path Query) over a named graph. Replaces the
    /// `FROM` clause with a table-
    /// function reference, since RPQ is a relation-producing function.
    pub fn rpq(mut self, expr: &str, start: u64, graph: &str) -> Self {
        self.table = format!("rpq({}, {start}, {})", quote_str(expr), quote_str(graph));
        self
    }

    /// Build a graph traversal as a relation.
    pub fn traverse(mut self, graph: &str, start: u64, label: Option<&str>, max_hops: u32) -> Self {
        let lbl = match label {
            Some(s) => quote_str(s),
            None => "NULL".into(),
        };
        self.table = format!(
            "traverse_match({}, {start}, {lbl}, {max_hops})",
            quote_str(graph)
        );
        self
    }

    /// Build a temporally bounded graph traversal.
    pub fn temporal_traverse(
        mut self,
        graph: &str,
        start: u64,
        label: Option<&str>,
        max_hops: u32,
        t_min: f64,
        t_max: f64,
    ) -> Self {
        let lbl = match label {
            Some(s) => quote_str(s),
            None => "NULL".into(),
        };
        self.table = format!(
            "temporal_traverse({}, {start}, {lbl}, {max_hops}, {t_min}, {t_max})",
            quote_str(graph)
        );
        self
    }

    /// Add a `uqa_highlight(field, query [, start_tag, end_tag,
    /// max_fragments, fragment_size])` projection.
    ///
    /// # Errors
    ///
    /// Returns an error when `field` is empty.
    pub fn highlight(mut self, field: &str, query: &str) -> Result<Self, SQLError> {
        validate_field_name("uqa_highlight", field)?;
        self.projections
            .push(format!("uqa_highlight({field}, {})", quote_str(query)));
        Ok(self)
    }

    /// `uqa_facets(field [, field2, ...])` projection. Fields are emitted as
    /// column references because the engine groups the values in those
    /// columns; string literals would facet the field names themselves.
    ///
    /// # Errors
    ///
    /// Returns an error when no fields are supplied or a field name is empty.
    pub fn facets(mut self, fields: &[&str]) -> Result<Self, SQLError> {
        if fields.is_empty() {
            return Err(SQLError::BadArity {
                name: "uqa_facets".into(),
                expected: ">=1 field".into(),
                actual: 0,
            });
        }
        for field in fields {
            validate_field_name("uqa_facets", field)?;
        }
        let inner = fields.join(", ");
        self.projections.push(format!("uqa_facets({inner})"));
        Ok(self)
    }

    /// Add a `deep_learn(model, training_set)` analytical training projection.
    pub fn deep_learn(mut self, model: &str, training_set: &str) -> Self {
        self.projections.push(format!(
            "deep_learn({}, {})",
            quote_str(model),
            quote_str(training_set)
        ));
        self
    }

    /// `bayesian_match(field, '<query>')` filter with the configured
    /// query-level BM25 score transform. Probability-calibration claims
    /// require parameters fitted and verified on held-out labels.
    pub fn bayesian_match(self, field: &str, query: &str) -> Self {
        self.r#where(format!("bayesian_match({field}, {})", quote_str(query)))
    }

    /// Bayesian BM25 with a document-level external prior. Rust's SQL
    /// builder takes the serializable prior shape (`prior_field`,
    /// `prior_mode`) rather than a closure, so the assembled query can
    /// still flow through the SQL engine.
    pub fn score_bayesian_with_prior(
        self,
        query: &str,
        field: Option<&str>,
        prior_field: Option<&str>,
        prior_mode: Option<&str>,
    ) -> Result<Self, SQLError> {
        let Some(prior_field) = prior_field else {
            return Err(SQLError::TypeMismatch("prior_fn is required".into()));
        };
        let Some(prior_mode) = prior_mode else {
            return Err(SQLError::TypeMismatch("prior_fn is required".into()));
        };
        let field = field.unwrap_or("_default");
        Ok(self.r#where(format!(
            "bayesian_match_with_prior({field}, {}, {prior_field}, {})",
            quote_str(query),
            quote_str(prior_mode)
        )))
    }

    pub fn learn_params(
        &self,
        query: &str,
        labels: &[u8],
        field: Option<&str>,
    ) -> Result<std::collections::BTreeMap<String, f64>, SQLError> {
        self.engine
            .learn_scoring_params(&self.table, field.unwrap_or("_default"), query, labels)
    }

    pub fn sparse_threshold(mut self, threshold: f64) -> Result<Self, SQLError> {
        if self.filters.len() != 1 {
            return Err(SQLError::TypeMismatch(
                "sparse_threshold requires a source and accepts exactly one retrieval filter"
                    .into(),
            ));
        }
        if !threshold.is_finite() {
            return Err(SQLError::TypeMismatch(format!(
                "sparse_threshold must be finite, got {threshold:?}"
            )));
        }
        let source = self.filters.pop().ok_or_else(|| {
            SQLError::Internal("validated sparse_threshold source disappeared".into())
        })?;
        self.filters = vec![format!("sparse_threshold({source}, {threshold})")];
        Ok(self)
    }

    /// Run `EXPLAIN <assembled SELECT>` and return the planner's rendered plan
    /// as a single string.
    pub fn explain(&self) -> Result<String, SQLError> {
        let stmt = self.to_sql();
        let result = self.engine.sql(&format!("EXPLAIN {stmt}"), &[])?;
        let mut out = String::new();
        for row in &result.rows {
            if let Some(Value::Str(line)) = row.get("plan") {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(line);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn assembles_basic_select() {
        // We can't make an Engine in unit tests without bringing in
        // its full surface, but to_sql doesn't touch the engine when
        // `execute()` isn't called. Use a transmute-free dummy via
        // `unsafe { std::mem::zeroed() }`? Avoid that; instead test
        // through a dedicated integration test that holds a real
        // Engine. The unit tests below verify the SQL builders that
        // don't need a runtime: render_value and quote_str.
    }

    #[test]
    fn render_int_and_string() {
        assert_eq!(render_value(&Value::Int(7)).unwrap(), "7");
        assert_eq!(render_value(&Value::Str("hi".into())).unwrap(), "'hi'");
    }

    #[test]
    fn render_string_escapes_single_quote() {
        assert_eq!(quote_str("it's"), "'it''s'");
    }

    #[test]
    fn render_list_uses_array_literal() {
        let v = Value::List(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(render_value(&v).unwrap(), "ARRAY[1, 2]");
    }

    #[test]
    fn mixed_numeric_arrow_output_does_not_round_large_integers() {
        let result = SQLResult::from_rows(
            vec!["value".into()],
            vec![
                BTreeMap::from([("value".into(), Value::Int(i64::MAX))]),
                BTreeMap::from([("value".into(), Value::Float(1.5))]),
            ],
        );
        assert_eq!(infer_arrow_type("value", &result), DataType::Utf8);
        let batch = sql_result_to_record_batch(&result).expect("lossless string batch");
        let values = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("utf8 array");
        assert_eq!(values.value(0), i64::MAX.to_string());
    }

    #[test]
    fn forced_metadata_type_mismatch_is_an_arrow_error() {
        let result = SQLResult::from_rows(
            vec!["_doc_id".into()],
            vec![BTreeMap::from([(
                "_doc_id".into(),
                Value::Str("not an id".into()),
            )])],
        );
        let error = sql_result_to_record_batch(&result)
            .expect_err("a string document id must not silently become null");
        assert!(error.to_string().contains("int64 was inferred"));
    }
}
