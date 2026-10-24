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
    /// Original parser diagnostics, including its one-based character position.
    #[error("{0}")]
    ParseDiagnostic(Box<pg_query::Diagnostic>),
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
    #[error(
        "{function_name}: column `{table}.{field}` has no text index; create one with CREATE INDEX ... ON {table} USING gin ({field})"
    )]
    TextIndexRequired {
        function_name: String,
        table: String,
        field: String,
    },
    #[error("{0}")]
    Cancelled(#[from] uqa_core::QueryCancelled),
    /// An owned host callback diagnostic. Its typed cause survives workers,
    /// routine context, cleanup and cloning without a thread-local side channel.
    #[error("{0}")]
    Host(#[source] std::sync::Arc<dyn std::error::Error + Send + Sync>),
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
    /// Routine traceback, independent of the primary diagnostic and its typed cause.
    #[error("{source}")]
    Context { source: Box<Self>, context: String },
    /// A failed cleanup attached to the original, typed SQL error. The primary
    /// message and SQLSTATE remain unchanged; DETAIL includes both diagnostics.
    #[error("{source}")]
    Cleanup {
        source: Box<Self>,
        failures: Vec<(String, Self)>,
        detail: String,
    },
    /// Source position independent of the primary diagnostic and its typed cause.
    #[error("{source}")]
    Position { source: Box<Self>, position: u32 },
    /// A routine-body cursor that cannot be transposed into the submitted SQL.
    #[error("{source}")]
    InternalPosition {
        source: Box<Self>,
        position: u32,
        query: String,
    },
    #[error("internal error: {0}")]
    Internal(String),
}

impl SQLError {
    /// Stable application error code, separate from the five-character SQLSTATE. Ordinary SQL errors do not acquire an application code.
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::NotificationRequiresSubscription => Some("NOTIFICATION_REQUIRES_SUBSCRIPTION"),
            Self::Context { source, .. }
            | Self::Cleanup { source, .. }
            | Self::Position { source, .. }
            | Self::InternalPosition { source, .. } => source.code(),
            _ => None,
        }
    }

    /// Append an enclosing routine frame after the innermost error context.
    pub fn with_context(self, frame: impl AsRef<str>) -> Self {
        match self {
            Self::Cleanup {
                source,
                failures,
                detail,
            } => Self::Cleanup {
                source: Box::new(source.with_context(frame)),
                failures,
                detail,
            },
            Self::InternalPosition {
                source,
                position,
                query,
            } => Self::InternalPosition {
                source: Box::new(source.with_context(frame)),
                position,
                query,
            },
            Self::Position { source, position } => Self::Position {
                source: Box::new(source.with_context(frame)),
                position,
            },
            Self::Context {
                source,
                mut context,
            } => {
                context.push('\n');
                context.push_str(frame.as_ref());
                Self::Context { source, context }
            }
            source => Self::Context {
                source: Box::new(source),
                context: frame.as_ref().to_owned(),
            },
        }
    }

    /// `PostgreSQL` CONTEXT, with inner frames before their callers.
    pub fn context(&self) -> Option<&str> {
        match self {
            Self::Context { context, .. } => Some(context),
            Self::Cleanup { source, .. }
            | Self::Position { source, .. }
            | Self::InternalPosition { source, .. } => source.context(),
            _ => None,
        }
    }

    /// The typed primary error, unaffected by an attached routine traceback.
    pub fn primary(&self) -> &Self {
        match self {
            Self::Context { source, .. }
            | Self::Cleanup { source, .. }
            | Self::Position { source, .. }
            | Self::InternalPosition { source, .. } => source.primary(),
            _ => self,
        }
    }

    /// Preserve the innermost reference's source position without changing its typed diagnostic.
    pub fn at_position(self, position: Option<u32>) -> Self {
        match position.filter(|position| *position != 0) {
            Some(position) if self.position().is_none() && self.internal_position().is_none() => {
                Self::Position {
                    source: Box::new(self),
                    position,
                }
            }
            _ => self,
        }
    }

    /// Position within `internal_query`, separate from the submitted SQL cursor.
    pub fn internal_position(&self) -> Option<u32> {
        match self {
            Self::InternalPosition { position, .. } => Some(*position),
            Self::Context { source, .. }
            | Self::Cleanup { source, .. }
            | Self::Position { source, .. } => source.internal_position(),
            _ => None,
        }
    }

    /// Routine source for an internal diagnostic cursor.
    pub fn internal_query(&self) -> Option<&str> {
        match self {
            Self::InternalPosition { query, .. } => Some(query),
            Self::Context { source, .. }
            | Self::Cleanup { source, .. }
            | Self::Position { source, .. } => source.internal_query(),
            _ => None,
        }
    }

    pub(crate) fn in_routine_source(
        self,
        body: &str,
        origin: Option<&crate::ast::RoutineBodyOrigin>,
    ) -> Self {
        let Some(position) = self.position() else {
            return self;
        };
        let source = self.without_position();
        match origin.and_then(|origin| origin.position(position)) {
            Some(position) => source.at_position(Some(position)),
            None => Self::InternalPosition {
                source: Box::new(source),
                position,
                query: body.to_owned(),
            },
        }
    }

    fn without_position(self) -> Self {
        match self {
            Self::Position { source, .. } => source.without_position(),
            Self::Context { source, context } => Self::Context {
                source: Box::new(source.without_position()),
                context,
            },
            Self::Cleanup {
                source,
                failures,
                detail,
            } => Self::Cleanup {
                source: Box::new(source.without_position()),
                failures,
                detail,
            },
            Self::ParseDiagnostic(mut diagnostic) => {
                diagnostic.cursor_position = 0;
                Self::ParseDiagnostic(diagnostic)
            }
            _ => self,
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
            SQLError::Context { source, .. }
            | SQLError::Cleanup { source, .. }
            | SQLError::Position { source, .. }
            | SQLError::InternalPosition { source, .. } => source.sqlstate(),
            SQLError::Cancelled(cancelled) => Some(cancelled.sqlstate()),
            SQLError::Host(_) => Some("38000"), // external_routine_exception
            SQLError::Parse(_) => Some("42601"), // syntax_error
            SQLError::ParseDiagnostic(diagnostic) => Some(&diagnostic.sqlstate),
            SQLError::Unsupported(_) | SQLError::NotificationRequiresSubscription => Some("0A000"), // feature_not_supported
            SQLError::UnknownTable(_) => Some("42P01"), // undefined_table
            SQLError::UnknownColumn(_) => Some("42703"), // undefined_column
            SQLError::AmbiguousColumn(_) => Some("42702"), // ambiguous_column
            SQLError::UnknownFunction(_) => Some("42883"), // undefined_function
            SQLError::TypeMismatch(_) | SQLError::TextIndexRequired { .. } => Some("42804"), // datatype_mismatch
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
            SQLError::Context { source, .. }
            | SQLError::Position { source, .. }
            | SQLError::InternalPosition { source, .. } => source.detail(),
            SQLError::Diagnostic { detail, .. } => detail.as_deref(),
            SQLError::Cleanup { detail, .. } => Some(detail),
            SQLError::ParseDiagnostic(diagnostic) => diagnostic.detail.as_deref(),
            _ => None,
        }
    }

    /// `PostgreSQL` HINT field, reported separately from the primary message.
    pub fn hint(&self) -> Option<&str> {
        match self {
            SQLError::Context { source, .. }
            | SQLError::Cleanup { source, .. }
            | SQLError::Position { source, .. }
            | SQLError::InternalPosition { source, .. } => source.hint(),
            SQLError::Diagnostic { hint, .. } => hint.as_deref(),
            SQLError::ParseDiagnostic(diagnostic) => diagnostic.hint.as_deref(),
            _ => None,
        }
    }

    /// One-based character position in the original submitted SQL.
    pub fn position(&self) -> Option<u32> {
        match self {
            Self::Context { source, .. } | Self::Cleanup { source, .. } => source.position(),
            Self::Position { position, .. } => Some(*position),
            Self::ParseDiagnostic(diagnostic) => u32::try_from(diagnostic.cursor_position)
                .ok()
                .filter(|position| *position != 0),
            _ => None,
        }
    }
}

mod cleanup;

pub type Result<T> = std::result::Result<T, SQLError>;

impl From<pg_query::Error> for SQLError {
    fn from(value: pg_query::Error) -> Self {
        match value {
            pg_query::Error::ParseDiagnostic(diagnostic) => Self::ParseDiagnostic(diagnostic),
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

#[cfg(test)]
mod tests {
    #[test]
    fn host_cause_survives_clone_context_position_and_cleanup() {
        #[derive(Debug, thiserror::Error)]
        #[error("host diagnostic")]
        struct Failure(u32);
        let error = super::SQLError::Host(std::sync::Arc::new(Failure(73)))
            .with_context("routine frame")
            .at_position(Some(11))
            .with_cleanup(
                super::SQLError::Internal("cleanup failed".into()),
                "close host state",
            );
        let cloned = error.clone();
        drop(error);
        let super::SQLError::Host(source) = cloned.primary() else {
            panic!("typed host cause lost")
        };
        assert_eq!(source.downcast_ref::<Failure>().unwrap().0, 73);
        assert_eq!(
            std::error::Error::source(cloned.primary())
                .unwrap()
                .downcast_ref::<std::sync::Arc<dyn std::error::Error + Send + Sync>>()
                .unwrap()
                .downcast_ref::<Failure>()
                .unwrap()
                .0,
            73
        );
        assert_eq!(cloned.sqlstate(), Some("38000"));
        assert_eq!(cloned.to_string(), "host diagnostic");
        assert_eq!(cloned.context(), Some("routine frame"));
        assert_eq!(cloned.position(), Some(11));
        assert!(cloned.detail().unwrap().contains("cleanup failed"));
    }

    #[test]
    fn source_positions_preserve_typed_errors_and_nested_context() {
        let error = super::SQLError::Diagnostic {
            sqlstate: "42P10".into(),
            message: "invalid reference".into(),
            detail: Some("invalid join type".into()),
            hint: Some("hint".into()),
        }
        .with_context("inner")
        .at_position(Some(17))
        .with_context("outer")
        .at_position(Some(29));
        assert_eq!(error.position(), Some(17));
        assert_eq!(error.context(), Some("inner\nouter"));
        assert_eq!(error.sqlstate(), Some("42P10"));
        assert_eq!(error.to_string(), "invalid reference");
        assert_eq!(error.detail(), Some("invalid join type"));
        assert_eq!(error.hint(), Some("hint"));
        assert!(matches!(
            error.primary(),
            super::SQLError::Diagnostic { .. }
        ));
        let cancelled =
            super::SQLError::Cancelled(uqa_core::QueryCancelled::USER_REQUEST).at_position(Some(3));
        assert!(matches!(cancelled.primary(), super::SQLError::Cancelled(_)));
        let subscription = super::SQLError::NotificationRequiresSubscription.at_position(Some(2));
        assert_eq!(
            subscription.code(),
            Some("NOTIFICATION_REQUIRES_SUBSCRIPTION")
        );
    }

    use super::SQLError;

    #[test]
    fn routine_frames_preserve_primary_fields_and_typed_cancellation() {
        let error = SQLError::Diagnostic {
            sqlstate: "22012".into(),
            message: "division by zero".into(),
            detail: Some("original detail".into()),
            hint: Some("original hint".into()),
        }
        .with_context("inner routine")
        .with_context("outer routine");
        assert_eq!(error.to_string(), "division by zero");
        assert_eq!(error.sqlstate(), Some("22012"));
        assert_eq!(error.detail(), Some("original detail"));
        assert_eq!(error.hint(), Some("original hint"));
        assert_eq!(error.context(), Some("inner routine\nouter routine"));
        assert!(matches!(error.primary(), SQLError::Diagnostic { .. }));

        let cancelled = SQLError::Cancelled(uqa_core::QueryCancelled::USER_REQUEST)
            .with_context("interrupted routine");
        assert_eq!(cancelled.sqlstate(), Some("57014"));
        assert!(matches!(cancelled.primary(), SQLError::Cancelled(_)));
    }
}
