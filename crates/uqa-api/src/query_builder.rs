//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `QueryBuilder` - assembles a SELECT statement and runs it via
//! [`uqa_engine::Engine::sql`]. Each method returns the builder by
//! value so calls compose linearly. The builder is transport-only;
//! all real work happens in the engine's SQL pipeline.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};
use uqa_sql::SQLError;

use std::fmt;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{ArrowError, DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::errors::ParquetError;

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
    order_by: Vec<(String, Order)>,
    limit: Option<u64>,
    offset: Option<u64>,
}

impl<'a> QueryBuilder<'a> {
    pub fn new(engine: &'a Engine, table: impl Into<String>) -> Self {
        Self {
            engine,
            table: table.into(),
            projections: Vec::new(),
            filters: Vec::new(),
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
    pub fn where_eq(self, column: &str, value: &Value) -> Self {
        let rendered = format!("{column} = {}", render_value(value));
        self.r#where(rendered)
    }

    /// Convenience: `<column> > <value>` filter.
    pub fn where_gt(self, column: &str, value: &Value) -> Self {
        self.r#where(format!("{column} > {}", render_value(value)))
    }

    /// Convenience: `<column> >= <value>` filter.
    pub fn where_gte(self, column: &str, value: &Value) -> Self {
        self.r#where(format!("{column} >= {}", render_value(value)))
    }

    /// Convenience: `<column> < <value>` filter.
    pub fn where_lt(self, column: &str, value: &Value) -> Self {
        self.r#where(format!("{column} < {}", render_value(value)))
    }

    /// Convenience: `<column> <= <value>` filter.
    pub fn where_lte(self, column: &str, value: &Value) -> Self {
        self.r#where(format!("{column} <= {}", render_value(value)))
    }

    /// Add `text_match(field, '<query>')` to WHERE.
    pub fn text_match(self, field: &str, query: &str) -> Self {
        self.r#where(format!("text_match({field}, {})", quote_str(query)))
    }

    /// Add `knn_match(field, ARRAY[v1, v2, ...], k)` to WHERE.
    pub fn knn_match(self, field: &str, vector: &[f32], k: usize) -> Self {
        let arr = vector
            .iter()
            .map(f32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        self.r#where(format!("knn_match({field}, ARRAY[{arr}], {k})"))
    }

    /// Add `multi_field_match(field_1, '<q1>', field_2, '<q2>', ...)` to WHERE.
    pub fn multi_field_match(self, fields_and_queries: &[(&str, &str)]) -> Self {
        let mut parts = Vec::with_capacity(fields_and_queries.len() * 2);
        for (field, query) in fields_and_queries {
            parts.push((*field).to_string());
            parts.push(quote_str(query));
        }
        self.r#where(format!("multi_field_match({})", parts.join(", ")))
    }

    /// Add `staged_retrieval(f1, '<q1>', k1, f2, '<q2>', k2, ...)` to WHERE.
    pub fn staged_retrieval(self, stages: &[(&str, &str, usize)]) -> Self {
        let mut parts = Vec::with_capacity(stages.len() * 3);
        for (field, query, top_k) in stages {
            parts.push((*field).to_string());
            parts.push(quote_str(query));
            parts.push(top_k.to_string());
        }
        self.r#where(format!("staged_retrieval({})", parts.join(", ")))
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
        use std::fmt::Write as _;
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

    /// Execute the query and convert the result to an Arrow
    /// [`RecordBatch`]. The batch always includes compatibility
    /// metadata columns `_doc_id` and `_score` before the requested
    /// projections.
    pub fn execute_arrow(&self) -> Result<RecordBatch, QueryBuilderError> {
        let result = self.execute_with_result_metadata()?;
        sql_result_to_record_batch(&result).map_err(QueryBuilderError::Arrow)
    }

    /// Execute the query and write the Arrow result to a Parquet file.
    pub fn execute_parquet<P: AsRef<Path>>(&self, path: P) -> Result<(), QueryBuilderError> {
        let batch = self.execute_arrow()?;
        let file = File::create(path).map_err(QueryBuilderError::Io)?;
        let mut writer =
            ArrowWriter::try_new(file, batch.schema(), None).map_err(QueryBuilderError::Parquet)?;
        writer.write(&batch).map_err(QueryBuilderError::Parquet)?;
        writer.close().map_err(QueryBuilderError::Parquet)?;
        Ok(())
    }

    fn execute_with_result_metadata(&self) -> Result<SQLResult, SQLError> {
        let mut builder = self.clone();
        let original = if builder.projections.is_empty() {
            vec!["*".to_string()]
        } else {
            builder.projections
        };
        builder.projections = Vec::with_capacity(original.len() + 2);
        push_projection_once(&mut builder.projections, "_doc_id");
        push_projection_once(&mut builder.projections, "_score");
        for projection in original {
            if projection != "_doc_id" && projection != "_score" {
                builder.projections.push(projection);
            }
        }
        builder.execute()
    }

    // -----------------------------------------------------------------
    // Convenience methods porting `uqa.api.query_builder.QueryBuilder`.
    // -----------------------------------------------------------------

    /// Add a bare term filter (`text_match(field, 'term')`). When
    /// `field` is `None`, the SQL function falls back to all-field
    /// search via the engine's analyzer registry.
    pub fn term(self, term: &str, field: Option<&str>) -> Self {
        match field {
            Some(f) => self.text_match(f, term),
            None => self.r#where(format!("text_match({})", quote_str(term))),
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

    /// Add a vector similarity filter via the
    /// `vector_similarity_match` SQL function.
    pub fn vector(self, query: &[f32], threshold: f32, field: &str) -> Self {
        let arr = render_vector(query);
        self.r#where(format!(
            "vector_similarity_match({field}, ARRAY[{arr}], {threshold})"
        ))
    }

    /// Promote the projection list to a single aggregate over `field`,
    /// e.g. `SELECT count(field) FROM ...`. Resets any previous
    /// projections.
    pub fn aggregate(mut self, field: &str, agg: &str) -> Self {
        self.projections.clear();
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
        // The engine's grouping picks the leading non-aggregate column
        // automatically; for explicitness we surface a deterministic
        // ORDER BY on the count.
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

    /// Add a `fuse_log_odds(...)` projection over a list of signals.
    /// `signals` is a sequence of pre-rendered SQL expressions (e.g.
    /// `text_match(...)`, `knn_match(...)`); the wrapper adds the
    /// `alpha` argument verbatim.
    pub fn fuse_log_odds(mut self, signals: &[&str], alpha: f64) -> Self {
        let inner = signals.join(", ");
        self.projections
            .push(format!("fuse_log_odds({inner}, {alpha})"));
        self
    }

    /// Add a `multi_stage(...)` projection that mirrors the canonical UQA implementation's
    /// `multi_stage` builder. Each stage is `(signal, top_k)`.
    pub fn multi_stage(mut self, stages: &[(&str, usize)]) -> Self {
        let mut parts: Vec<String> = Vec::with_capacity(stages.len() * 2);
        for (signal, top_k) in stages {
            parts.push((*signal).to_string());
            parts.push(top_k.to_string());
        }
        self.projections
            .push(format!("multi_stage({})", parts.join(", ")));
        self
    }

    /// Multi-signal attention fusion. `signals` are pre-rendered SQL
    /// expressions like `text_match('q')`, `knn_match('field', '[..]', k)`.
    /// Matches UQA behavior for `QueryBuilder.fuse_attention`.
    pub fn fuse_attention(mut self, signals: &[&str]) -> Self {
        self.projections
            .push(format!("attention({})", signals.join(", ")));
        self
    }

    /// Learned per-feature fusion using a saved `LearnedFusion` model.
    /// Matches UQA behavior for `QueryBuilder.fuse_learned`.
    pub fn fuse_learned(mut self, model: &str, signals: &[&str]) -> Self {
        let mut parts = vec![quote_str(model)];
        parts.extend(signals.iter().map(|s| (*s).to_string()));
        self.projections
            .push(format!("learned_fusion({})", parts.join(", ")));
        self
    }

    /// Calibrated KNN with cosine probabilities. Matches UQA behavior for
    /// `QueryBuilder.calibrated_vector_match`.
    pub fn calibrated_vector_match(
        mut self,
        field: &str,
        vector: &[f32],
        k: usize,
        threshold: Option<f32>,
    ) -> Self {
        let v = render_vector(vector);
        let proj = match threshold {
            Some(t) => format!(
                "calibrated_vector_match({}, ARRAY[{v}], {k}, {t})",
                quote_str(field)
            ),
            None => format!(
                "calibrated_vector_match({}, ARRAY[{v}], {k})",
                quote_str(field)
            ),
        };
        self.projections.push(proj);
        self
    }

    /// `RPQ` (Regular Path Query) over a named graph. Matches UQA behavior for
    /// `QueryBuilder.rpq`. Replaces the FROM clause with a table-
    /// function reference, since RPQ is a relation-producing function.
    pub fn rpq(mut self, expr: &str, start: u64, graph: &str) -> Self {
        self.table = format!("rpq({}, {start}, {})", quote_str(expr), quote_str(graph));
        self
    }

    /// Graph traversal as a relation. Matches UQA behavior for
    /// `QueryBuilder.traverse`.
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

    /// Temporally-bounded graph traversal. Matches UQA behavior for
    /// `QueryBuilder.temporal_traverse`.
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

    /// `uqa_highlight(field, query [, start_tag, end_tag, max_fragments,
    /// fragment_size])` projection. Matches UQA behavior for `QueryBuilder`'s
    /// highlight helper.
    pub fn highlight(mut self, field: &str, query: &str) -> Self {
        self.projections.push(format!(
            "uqa_highlight({}, {})",
            quote_str(field),
            quote_str(query)
        ));
        self
    }

    /// `uqa_facets(field [, field2, ...])` projection. Mirrors the canonical UQA implementation's
    /// facet builder.
    pub fn facets(mut self, fields: &[&str]) -> Self {
        let inner = fields
            .iter()
            .map(|f| quote_str(f))
            .collect::<Vec<_>>()
            .join(", ");
        self.projections.push(format!("uqa_facets({inner})"));
        self
    }

    /// `deep_learn(model, training_set)` projection. Mirrors the canonical UQA implementation's
    /// analytical training trigger.
    pub fn deep_learn(mut self, model: &str, training_set: &str) -> Self {
        self.projections.push(format!(
            "deep_learn({}, {})",
            quote_str(model),
            quote_str(training_set)
        ));
        self
    }

    /// `bayesian_match(field, '<query>')` filter - Bayesian BM25
    /// scoring with calibrated probabilities. Mirrors the canonical UQA implementation's
    /// `QueryBuilder.score_bayesian_bm25` style search.
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
        if self.filters.is_empty() {
            return Err(SQLError::TypeMismatch(
                "sparse_threshold requires a source".into(),
            ));
        }
        let source = self.filters.join(" AND ");
        self.filters = vec![format!("sparse_threshold({source}, {threshold})")];
        Ok(self)
    }

    /// Run `EXPLAIN <assembled SELECT>` and return the planner's
    /// rendered plan as a single string. Matches UQA behavior for
    /// `QueryBuilder.explain`.
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

fn render_vector(query: &[f32]) -> String {
    query
        .iter()
        .map(f32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug)]
pub enum QueryBuilderError {
    Sql(SQLError),
    Arrow(ArrowError),
    Parquet(ParquetError),
    Io(std::io::Error),
}

impl fmt::Display for QueryBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(err) => write!(f, "{err}"),
            Self::Arrow(err) => write!(f, "{err}"),
            Self::Parquet(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for QueryBuilderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(err) => Some(err),
            Self::Arrow(err) => Some(err),
            Self::Parquet(err) => Some(err),
            Self::Io(err) => Some(err),
        }
    }
}

impl From<SQLError> for QueryBuilderError {
    fn from(value: SQLError) -> Self {
        Self::Sql(value)
    }
}

fn push_projection_once(projections: &mut Vec<String>, projection: &str) {
    if !projections.iter().any(|p| p == projection) {
        projections.push(projection.to_string());
    }
}

fn sql_result_to_record_batch(result: &SQLResult) -> Result<RecordBatch, ArrowError> {
    let fields: Vec<Field> = result
        .columns
        .iter()
        .map(|column| Field::new(column, infer_arrow_type(column, result), true))
        .collect();
    let arrays: Vec<ArrayRef> = result
        .columns
        .iter()
        .zip(fields.iter())
        .map(|(column, field)| build_arrow_array(column, field.data_type(), result))
        .collect();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)
}

fn infer_arrow_type(column: &str, result: &SQLResult) -> DataType {
    if column == "_doc_id" {
        return DataType::Int64;
    }
    if column == "_score" {
        return DataType::Float64;
    }

    let mut ty: Option<DataType> = None;
    for row in &result.rows {
        let Some(value) = row.get(column) else {
            continue;
        };
        let next = match value {
            Value::Null => continue,
            Value::Bool(_) => DataType::Boolean,
            Value::Int(_) => DataType::Int64,
            Value::Float(_) => DataType::Float64,
            Value::Str(_)
            | Value::Bytes(_)
            | Value::Temporal(_)
            | Value::List(_)
            | Value::Map(_) => DataType::Utf8,
        };
        ty = Some(match (ty, next) {
            (None, dt) => dt,
            (Some(DataType::Int64), DataType::Float64)
            | (Some(DataType::Float64), DataType::Int64 | DataType::Float64) => DataType::Float64,
            (Some(current), dt) if current == dt => current,
            _ => DataType::Utf8,
        });
        if ty == Some(DataType::Utf8) {
            break;
        }
    }
    ty.unwrap_or(DataType::Utf8)
}

fn build_arrow_array(column: &str, data_type: &DataType, result: &SQLResult) -> ArrayRef {
    match data_type {
        DataType::Boolean => Arc::new(BooleanArray::from(
            result
                .rows
                .iter()
                .map(|row| match row.get(column) {
                    Some(Value::Bool(v)) => Some(*v),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )),
        DataType::Int64 => Arc::new(Int64Array::from(
            result
                .rows
                .iter()
                .map(|row| match row.get(column) {
                    Some(Value::Int(v)) => Some(*v),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )),
        DataType::Float64 => Arc::new(Float64Array::from(
            result
                .rows
                .iter()
                .map(|row| match row.get(column) {
                    Some(Value::Float(v)) => Some(*v),
                    Some(Value::Int(v)) => Some(*v as f64),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )),
        _ => Arc::new(StringArray::from(
            result
                .rows
                .iter()
                .map(|row| row.get(column).and_then(value_to_arrow_string))
                .collect::<Vec<_>>(),
        )),
    }
}

fn value_to_arrow_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(v) => Some(v.to_string()),
        Value::Int(v) => Some(v.to_string()),
        Value::Float(v) => Some(v.to_string()),
        Value::Str(v) => Some(v.clone()),
        Value::Bytes(v) => Some(format!("{v:?}")),
        Value::Temporal(v) => Some(v.to_sql_string()),
        Value::List(v) => Some(format!("{v:?}")),
        Value::Map(v) => Some(format!("{v:?}")),
    }
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Null | Value::Map(_) => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Str(s) => quote_str(s),
        Value::Bytes(b) => quote_str(&format!("<{} bytes>", b.len())),
        Value::Temporal(t) => quote_str(&t.to_sql_string()),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(render_value).collect();
            format!("ARRAY[{}]", inner.join(", "))
        }
    }
}

fn quote_str(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
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
        assert_eq!(render_value(&Value::Int(7)), "7");
        assert_eq!(render_value(&Value::Str("hi".into())), "'hi'");
    }

    #[test]
    fn render_string_escapes_single_quote() {
        assert_eq!(quote_str("it's"), "'it''s'");
    }

    #[test]
    fn render_list_uses_array_literal() {
        let v = Value::List(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(render_value(&v), "ARRAY[1, 2]");
    }
}
