//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct, authenticated HTTP access to local and Cloud UQA data planes.
//!
//! [`HttpEngine`] mirrors the embedded engine's asynchronous SQL-shaped surface. It can use an
//! explicit URL and token, environment variables, or the installed `uqa` CLI to resolve a local
//! or Cloud project once during construction.

mod cli_connection;
mod http_engine;
mod http_engine_error;
mod server_error_envelope;
mod sql_batch_execution;
mod sql_execution;
mod sql_parameter;
mod sql_statement;
mod sql_stream;
mod sql_stream_frame;

pub use http_engine::HttpEngine;
pub use http_engine_error::HttpEngineError;
pub use secrecy::SecretString;
pub use sql_batch_execution::SQLBatchExecution;
pub use sql_execution::SQLExecution;
pub use sql_statement::SQLStatement;
pub use sql_stream::SQLStream;
pub use sql_stream_frame::SQLStreamFrame;
pub use uqa_sql::{AsyncSQLEngine, SQLParam, SQLResult};
