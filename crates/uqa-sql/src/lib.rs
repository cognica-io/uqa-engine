//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL parser and compiler: `PostgreSQL` grammar via `libpg_query`, UQA
//! function registry, expression evaluator, FTS query mini-language.

#![allow(
    clippy::useless_format,
    clippy::manual_let_else,
    clippy::match_wildcard_for_single_variants,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::match_same_arms,
    clippy::unnested_or_patterns,
    clippy::unnecessary_join,
    clippy::unnecessary_map_or
)]

pub mod ast;
pub mod compiler;
pub mod error;
pub mod expr;
pub mod params;
pub mod registry;
pub mod result;

pub use ast::{ColumnType, Statement};
pub use compiler::{compile, plan_only_for_test};
pub use error::SqlError;
pub use params::SqlParam;
pub use result::{ResultRow, SqlResult};
