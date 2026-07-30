//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-owned SQL function extension points.

use uqa_core::Value;
use uqa_sql::SQLError;

pub use uqa_sql::ast::FunctionVolatility as SQLFunctionVolatility;

/// Execution properties declared for a Rust-backed SQL callback.
///
/// The default is deliberately conservative: callbacks registered through the
/// original two-argument APIs are volatile and may mutate engine state. Callers
/// must opt in to read-only execution explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SQLFunctionOptions {
    /// Whether optimizer rewrites may duplicate, eliminate, or reorder calls.
    pub volatility: SQLFunctionVolatility,
    /// Whether invoking the callback can mutate engine or session state.
    pub may_mutate_engine: bool,
}

impl SQLFunctionOptions {
    #[must_use]
    pub const fn new(volatility: SQLFunctionVolatility, may_mutate_engine: bool) -> Self {
        Self {
            volatility,
            may_mutate_engine,
        }
    }

    /// Declare a callback that cannot mutate engine state.
    #[must_use]
    pub const fn read_only(volatility: SQLFunctionVolatility) -> Self {
        Self::new(volatility, false)
    }
}

impl Default for SQLFunctionOptions {
    fn default() -> Self {
        Self::new(SQLFunctionVolatility::Volatile, true)
    }
}

pub(crate) struct RegisteredSQLFunction<F: ?Sized> {
    pub(crate) function: std::sync::Arc<F>,
    pub(crate) options: SQLFunctionOptions,
}

impl<F: ?Sized> RegisteredSQLFunction<F> {
    pub(crate) fn new(function: std::sync::Arc<F>, options: SQLFunctionOptions) -> Self {
        Self { function, options }
    }
}

impl<F: ?Sized> Clone for RegisteredSQLFunction<F> {
    fn clone(&self) -> Self {
        Self {
            function: self.function.clone(),
            options: self.options,
        }
    }
}

/// Result shape returned by a registered SQL table function.
#[derive(Debug, Clone, PartialEq)]
pub struct SQLTableFunctionResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl SQLTableFunctionResult {
    pub fn new(
        columns: impl IntoIterator<Item = impl Into<String>>,
        rows: Vec<Vec<Value>>,
    ) -> Self {
        Self {
            columns: columns.into_iter().map(Into::into).collect(),
            rows,
        }
    }
}

/// Pull-based result returned by a registered SQL table function.
///
/// The iterator may report a late error after yielding earlier rows; the SQL
/// physical pipeline propagates that error instead of turning it into an empty
/// or truncated result.
pub struct SQLTableFunctionStream {
    pub columns: Vec<String>,
    pub rows: Box<dyn Iterator<Item = Result<Vec<Value>, SQLError>> + Send>,
}

impl SQLTableFunctionStream {
    pub fn new<I>(columns: impl IntoIterator<Item = impl Into<String>>, rows: I) -> Self
    where
        I: Iterator<Item = Result<Vec<Value>, SQLError>> + Send + 'static,
    {
        Self {
            columns: columns.into_iter().map(Into::into).collect(),
            rows: Box::new(rows),
        }
    }
}

impl From<SQLTableFunctionResult> for SQLTableFunctionStream {
    fn from(result: SQLTableFunctionResult) -> Self {
        Self {
            columns: result.columns,
            rows: Box::new(result.rows.into_iter().map(Ok)),
        }
    }
}

/// Rust implementation of a scalar SQL function.
pub trait SQLScalarFunction: Send + Sync {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError>;
}

impl<F> SQLScalarFunction for F
where
    F: Fn(&[Value]) -> Result<Value, SQLError> + Send + Sync,
{
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        self(args)
    }
}

/// Rust implementation of a SQL table function used in `FROM`.
pub trait SQLTableFunction: Send + Sync {
    fn call(&self, _args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        Err(SQLError::Unsupported(
            "table function implements only the streaming call interface".into(),
        ))
    }

    fn call_stream(&self, args: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        self.call(args).map(Into::into)
    }
}

impl<F> SQLTableFunction for F
where
    F: Fn(&[Value]) -> Result<SQLTableFunctionResult, SQLError> + Send + Sync,
{
    fn call(&self, args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        self(args)
    }
}

/// Factory for per-group SQL aggregate state.
pub trait SQLAggregateFunction: Send + Sync {
    fn create_state(&self) -> Box<dyn SQLAggregateState>;
}

impl<F, S> SQLAggregateFunction for F
where
    F: Fn() -> S + Send + Sync,
    S: SQLAggregateState + 'static,
{
    fn create_state(&self) -> Box<dyn SQLAggregateState> {
        Box::new(self())
    }
}

/// Per-group state for a registered SQL aggregate function.
pub trait SQLAggregateState: Send {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError>;
    fn finish(&self) -> Result<Value, SQLError>;
}
