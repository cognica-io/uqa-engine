//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-owned SQL function extension points.

use uqa_core::Value;
use uqa_sql::SQLError;

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
    fn call(&self, args: &[Value]) -> Result<SQLTableFunctionResult, SQLError>;
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
