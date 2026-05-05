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
    clippy::unnecessary_map_or,
    clippy::too_many_lines,
    clippy::needless_return,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::map_unwrap_or,
    clippy::manual_string_new,
    clippy::option_if_let_else
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
