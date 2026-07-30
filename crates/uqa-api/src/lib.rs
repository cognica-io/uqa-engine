//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fluent `QueryBuilder` API on top of the SQL surface.
//!
//! The builder composes a `SELECT` statement programmatically and
//! sends it through [`uqa_engine::Engine::sql`]. Infallible methods return
//! the builder by value; validated helpers return `Result` and remain
//! linearly chainable with `?`:
//!
//! ```no_run
//! use uqa_api::QueryBuilder;
//! use uqa_core::Value;
//! use uqa_engine::Engine;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let engine = Engine::new();
//! let result = QueryBuilder::new(&engine, "docs")
//!     .select_columns(&["id", "_score"])
//!     .where_gte("year", &Value::Int(2024))?
//!     .knn_match("embedding", &[0.1, 0.2], 5)?
//!     .order_by_desc("_score")
//!     .limit(5)
//!     .execute()?;
//! # let _ = result;
//! # Ok(())
//! # }
//! ```
//!
//! The implementation is deliberately thin: it builds a SQL string
//! and lets the existing parser/engine path do the actual work. Typed
//! helpers cover common read and retrieval flows; callers can use raw
//! projection and predicate fragments where appropriate, while complete SQL
//! remains available through [`uqa_engine::Engine::sql`].

pub mod query_builder;

pub use query_builder::{Order, QueryBuilder};

/// compatibility convenience: build a fluent [`QueryBuilder`] scoped
/// to `table` against `engine`. Mirrors the canonical UQA implementation's `Engine.query(table)`.
pub fn query(engine: &uqa_engine::Engine, table: impl Into<String>) -> QueryBuilder<'_> {
    QueryBuilder::new(engine, table)
}
