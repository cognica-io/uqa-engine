//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Error types surfaced by the SQL compiler and executor.

#[derive(Debug, thiserror::Error)]
pub enum SQLError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("unsupported SQL feature: {0}")]
    Unsupported(String),
    #[error("unknown table: {0}")]
    UnknownTable(String),
    #[error("unknown column: {0}")]
    UnknownColumn(String),
    #[error("unknown function: {0}")]
    UnknownFunction(String),
    #[error("type mismatch: {0}")]
    TypeMismatch(String),
    #[error("invalid argument count for `{name}`: expected {expected}, got {actual}")]
    BadArity {
        name: String,
        expected: String,
        actual: usize,
    },
    #[error("missing parameter ${0}")]
    MissingParam(usize),
    #[error("vector dimension mismatch: expected {expected}, got {actual}")]
    VectorDimMismatch { expected: usize, actual: usize },
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, SQLError>;

impl From<pg_query::Error> for SQLError {
    fn from(value: pg_query::Error) -> Self {
        SQLError::Parse(value.to_string())
    }
}
