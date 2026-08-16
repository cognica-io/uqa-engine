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
    clippy::option_if_let_else,
    clippy::cast_lossless,
    clippy::format_collect
)]

pub mod ast;
mod async_sql_engine;
pub mod compiler;
pub mod error;
pub mod expr;
pub mod fts_query;
pub mod params;
pub mod plpgsql;
pub mod registry;
pub mod result;

pub use ast::{ColumnType, Statement};
pub use async_sql_engine::AsyncSQLEngine;
pub use compiler::{compile, plan_only_for_test};
pub use error::SQLError;
pub use fts_query::{parse_query_string as parse_fts_query_string, tokenize as fts_tokenize};
pub use fts_query::{FTSNode, FTSParser, FTSToken, FTSTokenType};
pub use params::SQLParam;
pub use result::{ResultRow, SQLResult};
