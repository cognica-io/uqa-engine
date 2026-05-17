//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fluent `QueryBuilder` API on top of the SQL surface.
//!
//! The builder composes a `SELECT` statement programmatically and
//! sends it through [`uqa_engine::Engine::sql`]. Every clause method
//! returns the builder by value so calls chain naturally:
//!
//! ```ignore
//! use uqa_api::QueryBuilder;
//! let result = QueryBuilder::new(&engine, "docs")
//!     .select_columns(&["id", "_score"])
//!     .text_match("body", "rust")
//!     .order_by_desc("_score")
//!     .limit(5)
//!     .execute()?;
//! ```
//!
//! The implementation is deliberately thin: it builds a SQL string
//! and lets the existing parser/engine path do the actual work. That
//! keeps every feature already available through SQL — graph
//! functions, hybrid scoring, `deep_predict`, `multi_field_match`,
//! `staged_retrieval` — automatically reachable from the builder.

pub mod query_builder;

pub use query_builder::{Order, QueryBuilder};

/// compatibility convenience: build a fluent [`QueryBuilder`] scoped
/// to `table` against `engine`. Mirrors the canonical UQA implementation's `Engine.query(table)`.
pub fn query(engine: &uqa_engine::Engine, table: impl Into<String>) -> QueryBuilder<'_> {
    QueryBuilder::new(engine, table)
}
