//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL syntax, shared plan and scalar models, static schema and type binding, catalog definitions, routine signatures, value expressions, and the FTS query language. The parser uses the imported `PostgreSQL` grammar through `libpg_query`; analysis has no engine or physical execution dependency.

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
pub mod binding;
pub mod catalog;
pub mod compiler;
pub mod copy;
pub mod error;
pub mod expr;
pub mod fts_query;
pub mod ir;
pub mod params;
pub mod plan;
pub mod plpgsql;
pub mod registry;
pub mod render;
pub mod result;
pub mod routines;
pub mod schema;
pub mod semantics;
pub mod type_resolution;

pub use ast::{ColumnType, Statement};
pub use async_sql_engine::AsyncSQLEngine;
pub use compiler::{
    compile, parse_regobject_name, parse_regprocedure_name, parse_regtype_name, parse_statements,
    plan_only_for_test, resolve_deferred_create_foreign_table, resolve_deferred_create_table,
    ParsedRegprocedureName, ParsedRegtypeName, ParsedStatement,
};
pub use error::SQLError;
pub use fts_query::{parse_query_string as parse_fts_query_string, tokenize as fts_tokenize};
pub use fts_query::{FTSNode, FTSParser, FTSToken, FTSTokenType};
pub use params::SQLParam;
pub use result::{ResultRow, SQLResult, SQLResultKind};

pub use ir::{
    scalar_call_argument, scalar_call_arguments, ScalarExpr, ScalarFrameBound, ScalarOrder,
    ScalarWindowFrame, ScalarWindowSpec, SubqueryId,
};
pub use schema::{ColumnIdentity, RowSchema};

pub use type_resolution::*;
pub use uqa_core::RelationIdentity;

pub mod assignment;

pub mod maintenance;
