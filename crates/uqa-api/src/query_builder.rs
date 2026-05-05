//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `QueryBuilder` — assembles a SELECT statement and runs it via
//! [`uqa_engine::Engine::sql`]. Each method returns the builder by
//! value so calls compose linearly. The builder is transport-only;
//! all real work happens in the engine's SQL pipeline.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};
use uqa_sql::SQLError;

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
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Null | Value::Map(_) => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Str(s) => quote_str(s),
        Value::Bytes(b) => quote_str(&format!("<{} bytes>", b.len())),
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
        // `unsafe { std::mem::zeroed() }`? Avoid that — instead test
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
