//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime contracts for scalar, table, and aggregate extensions.

pub use crate::row_stream::{SQLRowSource, SQLRowStream};
use uqa_core::Value;
use uqa_sql::SQLError;

pub use uqa_sql::ast::FunctionVolatility as SQLFunctionVolatility;

/// Host-owned invocation data belonging to the calling session. Registrations
/// are shared across sessions; this data travels with execution, including
/// worker and retained-portal execution, instead of with the registered code.
/// Hosts should store weak session references here to avoid ownership cycles.
#[derive(Clone, Default)]
pub struct SQLFunctionContext(Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>);

impl SQLFunctionContext {
    #[must_use]
    pub const fn empty() -> Self {
        Self(None)
    }

    #[must_use]
    pub fn new<T: Send + Sync + 'static>(data: T) -> Self {
        Self(Some(std::sync::Arc::new(data)))
    }

    #[must_use]
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.0.as_ref()?.downcast_ref()
    }
}

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

pub struct RegisteredSQLFunction<F: ?Sized> {
    pub function: std::sync::Arc<F>,
    pub options: SQLFunctionOptions,
}

impl<F: ?Sized> RegisteredSQLFunction<F> {
    pub fn new(function: std::sync::Arc<F>, options: SQLFunctionOptions) -> Self {
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
    pub rows: SQLRowStream<'static, Vec<Value>>,
}

impl SQLTableFunctionStream {
    pub fn new<I>(columns: impl IntoIterator<Item = impl Into<String>>, rows: I) -> Self
    where
        I: Iterator<Item = Result<Vec<Value>, SQLError>> + Send + 'static,
    {
        Self {
            columns: columns.into_iter().map(Into::into).collect(),
            rows: SQLRowStream::new(rows),
        }
    }

    /// Transfer a scan with fallible cleanup to the execution pipeline.
    pub fn from_source(
        columns: impl IntoIterator<Item = impl Into<String>>,
        rows: impl SQLRowSource<Vec<Value>> + 'static,
    ) -> Self {
        Self {
            columns: columns.into_iter().map(Into::into).collect(),
            rows: SQLRowStream::from_source(rows),
        }
    }
}

impl From<SQLTableFunctionResult> for SQLTableFunctionStream {
    fn from(result: SQLTableFunctionResult) -> Self {
        Self {
            columns: result.columns,
            rows: SQLRowStream::new(result.rows.into_iter().map(Ok)),
        }
    }
}

/// Rust implementation of a scalar SQL function.
pub trait SQLScalarFunction: Send + Sync {
    fn call(&self, _args: &[Value]) -> Result<Value, SQLError> {
        Err(SQLError::Unsupported(
            "scalar function requires an invocation context".into(),
        ))
    }

    fn call_with_context(
        &self,
        _context: &SQLFunctionContext,
        args: &[Value],
    ) -> Result<Value, SQLError> {
        self.call(args)
    }
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
    /// Stable, nonempty output names, independent of argument values and scan creation.
    /// A declared descriptor permits binding correlated and empty sources
    /// without executing the callback. Every call must return these columns
    /// in this order. `None` retains the runtime-described call interface.
    fn output_columns(&self) -> Option<&[String]> {
        None
    }

    fn call(&self, _args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        Err(SQLError::Unsupported(
            "table function implements only the streaming call interface".into(),
        ))
    }

    fn call_stream(&self, args: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        self.call(args).map(Into::into)
    }

    fn call_with_context(
        &self,
        _context: &SQLFunctionContext,
        args: &[Value],
    ) -> Result<SQLTableFunctionResult, SQLError> {
        self.call(args)
    }

    fn call_stream_with_context(
        &self,
        _context: &SQLFunctionContext,
        args: &[Value],
    ) -> Result<SQLTableFunctionStream, SQLError> {
        self.call_stream(args)
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

/// Infallible factory retained for existing Rust aggregate implementations.
///
/// Every implementation also implements [`SQLAggregateFactory`]. A callback
/// whose state creation can fail implements that fallible interface directly.
pub trait SQLAggregateFunction: Send + Sync {
    fn create_state(&self) -> Box<dyn SQLAggregateState>;
}

/// Fallible creation of an independent SQL aggregate state.
///
/// Execution propagates creation failure before observing or finishing that
/// state, including a global aggregate over an empty input. A failed factory
/// remains responsible for resources it allocated before returning the error.
pub trait SQLAggregateFactory: Send + Sync {
    fn try_create_state(&self) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        Err(SQLError::Unsupported(
            "aggregate factory requires an invocation context".into(),
        ))
    }

    fn try_create_state_with_context(
        &self,
        _context: &SQLFunctionContext,
    ) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        self.try_create_state()
    }
}

impl<F: SQLAggregateFunction + ?Sized> SQLAggregateFactory for F {
    fn try_create_state(&self) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        Ok(self.create_state())
    }
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

    /// Release host resources after the last observation or value read. Execution
    /// calls this once, including on failure, before completing the statement.
    /// Resources must be released even when cleanup returns an error. `finish`
    /// remains non-consuming because a window can read the same state repeatedly.
    /// Existing Rust states may retain ordinary infallible `Drop` cleanup.
    fn close(&mut self) -> Result<(), SQLError> {
        Ok(())
    }
}

/// Typed lookup of registered aggregate factories during physical plan preparation.
pub trait AggregateFunctionRegistry:
    uqa_sql::FunctionTypeResolver + uqa_sql::plan::AggregateClassifier
{
    fn registered_aggregate_function(
        &self,
        name: &str,
    ) -> Option<std::sync::Arc<dyn SQLAggregateFactory>>;
}
