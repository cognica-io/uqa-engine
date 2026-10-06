//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Error types surfaced by the SQL compiler and executor.

#[derive(Debug, Clone, thiserror::Error)]
pub enum SQLError {
    #[error("{0}")]
    Parse(String),
    #[error("{0}")]
    Unsupported(String),
    /// The host configured this SQL session to require an independently owned notification subscription.
    #[error("LISTEN and UNLISTEN require a notification subscription")]
    NotificationRequiresSubscription,
    #[error("relation \"{0}\" does not exist")]
    UnknownTable(String),
    #[error("column \"{0}\" does not exist")]
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
    /// A primary SQL error with separate `PostgreSQL` diagnostic fields. `SQLERRM` and `Display` expose only the primary message; protocol clients receive detail and hint independently.
    #[error("{message}")]
    Diagnostic {
        sqlstate: String,
        message: String,
        detail: Option<String>,
        hint: Option<String>,
    },
    #[error("internal error: {0}")]
    Internal(String),
}

impl SQLError {
    /// Stable application error code, separate from the five-character SQLSTATE. Ordinary SQL errors do not acquire an application code.
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::NotificationRequiresSubscription => Some("NOTIFICATION_REQUIRES_SUBSCRIPTION"),
            _ => None,
        }
    }

    /// `ParseFuncOrColumn`'s error when no function matches a call: `signature` is the name with its argument types, as `func_signature_string` spells it.
    pub fn undefined_function_call(signature: &str) -> Self {
        Self::Diagnostic {
            sqlstate: "42883".into(),
            message: format!("function {signature} does not exist"),
            detail: None,
            hint: Some(
                "No function matches the given name and argument types. You might need to add explicit type casts."
                    .into(),
            ),
        }
    }

    /// `ParseFuncOrColumn`'s error when more than one function matches a call equally well.
    pub fn ambiguous_function_call(signature: &str) -> Self {
        Self::Diagnostic {
            sqlstate: "42725".into(),
            message: format!("function {signature} is not unique"),
            detail: None,
            hint: Some(
                "Could not choose a best candidate function. You might need to add explicit type casts."
                    .into(),
            ),
        }
    }

    /// A failed call resolution in `ParseFuncOrColumn`'s terms: `42883` when no function matches and `42725` when no candidate is best, each with its hint.
    pub fn function_call_resolution(sqlstate: &str, signature: &str, suffix: &str) -> Self {
        match (sqlstate, suffix) {
            ("42883", "does not exist") => Self::undefined_function_call(signature),
            ("42725", "is not unique") => Self::ambiguous_function_call(signature),
            _ => Self::Routine {
                sqlstate: sqlstate.into(),
                message: format!("function {signature} {suffix}"),
            },
        }
    }

    pub fn unknown_qualified_column(qualifier: &str, column: &str) -> Self {
        Self::Routine {
            sqlstate: "42703".into(),
            message: format!("column {qualifier}.{column} does not exist"),
        }
    }

    /// `PostgreSQL` `SQLSTATE` code for the error, mirroring the
    /// the current exception-to-state mapping. `None` for
    /// errors that do not carry a defined `SQLSTATE`.
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            SQLError::Cancelled(cancelled) => Some(cancelled.sqlstate()),
            SQLError::Parse(_) => Some("42601"), // syntax_error
            SQLError::Unsupported(_) | SQLError::NotificationRequiresSubscription => Some("0A000"), // feature_not_supported
            SQLError::UnknownTable(_) => Some("42P01"), // undefined_table
            SQLError::UnknownColumn(_) => Some("42703"), // undefined_column
            SQLError::AmbiguousColumn(_) => Some("42702"), // ambiguous_column
            SQLError::UnknownFunction(_) => Some("42883"), // undefined_function
            SQLError::TypeMismatch(_) => Some("42804"), // datatype_mismatch
            SQLError::BadArity { .. } => Some("42883"), // undefined_function (PG)
            SQLError::MissingParam(_) => Some("S1002"), // ERRCODE_INVALID_PARAMETER_VALUE
            SQLError::VectorDimMismatch { .. } => Some("22023"), // invalid_parameter_value
            SQLError::Routine { sqlstate, .. } | SQLError::Diagnostic { sqlstate, .. } => {
                Some(sqlstate)
            }
            SQLError::Internal(_) => Some("XX000"), // internal_error
        }
    }

    /// `PostgreSQL` DETAIL field, reported separately from the primary message.
    pub fn detail(&self) -> Option<&str> {
        match self {
            SQLError::Diagnostic { detail, .. } => detail.as_deref(),
            _ => None,
        }
    }

    /// `PostgreSQL` HINT field, reported separately from the primary message.
    pub fn hint(&self) -> Option<&str> {
        match self {
            SQLError::Diagnostic { hint, .. } => hint.as_deref(),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, SQLError>;

impl From<pg_query::Error> for SQLError {
    fn from(value: pg_query::Error) -> Self {
        match value {
            pg_query::Error::ParseDiagnostic(diagnostic) => Self::Diagnostic {
                sqlstate: diagnostic.sqlstate,
                message: diagnostic.message,
                detail: diagnostic.detail,
                hint: diagnostic.hint,
            },
            pg_query::Error::Parse(message) => Self::Parse(message),
            other => Self::Parse(other.to_string()),
        }
    }
}

impl From<uqa_core::memory::MemoryError> for SQLError {
    fn from(error: uqa_core::memory::MemoryError) -> Self {
        Self::Routine {
            sqlstate: "53200".into(),
            message: error.to_string(),
        }
    }
}

impl From<uqa_core::ValueRetentionError> for SQLError {
    fn from(error: uqa_core::ValueRetentionError) -> Self {
        match error {
            uqa_core::ValueRetentionError::Memory(error) => error.into(),
            uqa_core::ValueRetentionError::Cancelled(error) => error.into(),
            error @ uqa_core::ValueRetentionError::Malformed { .. } => Self::Routine {
                sqlstate: "XX001".into(),
                message: error.to_string(),
            },
        }
    }
}
