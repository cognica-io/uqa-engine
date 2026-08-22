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
    #[error("relation \"{0}\" does not exist")]
    UnknownTable(String),
    #[error("unknown column: {0}")]
    UnknownColumn(String),
    #[error("column reference \"{0}\" is ambiguous")]
    AmbiguousColumn(String),
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
    #[error("No value supplied for parameter ${0}")]
    MissingParam(usize),
    #[error("vector dimension mismatch: expected {expected}, got {actual}")]
    VectorDimMismatch { expected: usize, actual: usize },
    #[error("{0}")]
    Cancelled(#[from] uqa_core::QueryCancelled),
    /// Error raised by (or on behalf of) a user-defined SQL /
    /// `PL/pgSQL` routine. Carries an explicit `SQLSTATE` so
    /// `EXCEPTION WHEN <condition>` handlers and `SQLSTATE` /
    /// `SQLERRM` report the same code `PostgreSQL` would.
    #[error("{message}")]
    Routine { sqlstate: String, message: String },
    #[error("internal error: {0}")]
    Internal(String),
}

impl SQLError {
    /// `PostgreSQL` `SQLSTATE` code for the error, mirroring the
    /// the current exception-to-state mapping. `None` for
    /// errors that do not carry a defined `SQLSTATE`.
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            SQLError::Cancelled(_) => Some(uqa_core::SQLSTATE_QUERY_CANCELED),
            SQLError::Parse(_) => Some("42601"), // syntax_error
            SQLError::Unsupported(_) => Some("0A000"), // feature_not_supported
            SQLError::UnknownTable(_) => Some("42P01"), // undefined_table
            SQLError::UnknownColumn(_) => Some("42703"), // undefined_column
            SQLError::AmbiguousColumn(_) => Some("42702"), // ambiguous_column
            SQLError::UnknownFunction(_) => Some("42883"), // undefined_function
            SQLError::TypeMismatch(_) => Some("42804"), // datatype_mismatch
            SQLError::BadArity { .. } => Some("42883"), // undefined_function (PG)
            SQLError::MissingParam(_) => Some("S1002"), // ERRCODE_INVALID_PARAMETER_VALUE
            SQLError::VectorDimMismatch { .. } => Some("22023"), // invalid_parameter_value
            SQLError::Routine { sqlstate, .. } => Some(sqlstate),
            SQLError::Internal(_) => Some("XX000"), // internal_error
        }
    }
}

pub type Result<T> = std::result::Result<T, SQLError>;

impl From<pg_query::Error> for SQLError {
    fn from(value: pg_query::Error) -> Self {
        match value {
            pg_query::Error::Parse(message)
                if message == "WITH TIES cannot be specified without ORDER BY clause" =>
            {
                SQLError::Routine {
                    sqlstate: "42601".into(),
                    message,
                }
            }
            other => SQLError::Parse(other.to_string()),
        }
    }
}
