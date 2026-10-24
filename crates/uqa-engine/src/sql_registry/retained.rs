//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep registration identities alive for escaped scans and aggregate states.

use crate::{
    SQLAggregateFactory, SQLAggregateState, SQLError, SQLFunctionContext, SQLRowSource,
    SQLRowStream, SQLTableFunction, SQLTableFunctionResult, SQLTableFunctionStream, Value,
};
use std::sync::{Arc, Weak};

pub(super) struct Table<F> {
    function: F,
    owner: Weak<Self>,
}

impl<F: SQLTableFunction + 'static> Table<F> {
    pub(super) fn wrap(function: F) -> Arc<dyn SQLTableFunction> {
        Arc::new_cyclic(|owner| Self {
            function,
            owner: owner.clone(),
        })
    }
}

impl<F: SQLTableFunction + 'static> SQLTableFunction for Table<F> {
    fn call(&self, args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        self.function.call(args)
    }

    fn call_with_context(
        &self,
        context: &SQLFunctionContext,
        args: &[Value],
    ) -> Result<SQLTableFunctionResult, SQLError> {
        self.function.call_with_context(context, args)
    }

    fn call_stream(&self, args: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        Ok(self.retain(self.function.call_stream(args)?))
    }

    fn call_stream_with_context(
        &self,
        context: &SQLFunctionContext,
        args: &[Value],
    ) -> Result<SQLTableFunctionStream, SQLError> {
        Ok(self.retain(self.function.call_stream_with_context(context, args)?))
    }
}

impl<F: SQLTableFunction + 'static> Table<F> {
    fn retain(&self, stream: SQLTableFunctionStream) -> SQLTableFunctionStream {
        SQLTableFunctionStream::from_source(
            stream.columns,
            Rows {
                rows: stream.rows,
                _registration: self
                    .owner
                    .upgrade()
                    .expect("invoked table registration is alive"),
            },
        )
    }
}

struct Rows {
    rows: SQLRowStream<'static, Vec<Value>>,
    _registration: Arc<dyn SQLTableFunction>,
}

impl SQLRowSource<Vec<Value>> for Rows {
    fn next_row(&mut self) -> Result<Option<Vec<Value>>, SQLError> {
        self.rows.next().transpose()
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.rows.close()
    }
}

pub(super) struct Aggregate<F> {
    function: F,
    owner: Weak<Self>,
}

impl<F: SQLAggregateFactory + 'static> Aggregate<F> {
    pub(super) fn wrap(function: F) -> Arc<dyn SQLAggregateFactory> {
        Arc::new_cyclic(|owner| Self {
            function,
            owner: owner.clone(),
        })
    }

    fn retain(&self, state: Box<dyn SQLAggregateState>) -> Box<dyn SQLAggregateState> {
        Box::new(State {
            state: Some(state),
            _registration: self
                .owner
                .upgrade()
                .expect("invoked aggregate registration is alive"),
        })
    }
}

impl<F: SQLAggregateFactory + 'static> SQLAggregateFactory for Aggregate<F> {
    fn try_create_state(&self) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        Ok(self.retain(self.function.try_create_state()?))
    }

    fn try_create_state_with_context(
        &self,
        context: &SQLFunctionContext,
    ) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        Ok(self.retain(self.function.try_create_state_with_context(context)?))
    }
}

struct State {
    state: Option<Box<dyn SQLAggregateState>>,
    _registration: Arc<dyn SQLAggregateFactory>,
}

impl SQLAggregateState for State {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        self.state.as_mut().ok_or_else(closed)?.observe(args)
    }

    fn finish(&self) -> Result<Value, SQLError> {
        self.state.as_ref().ok_or_else(closed)?.finish()
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.state.take().map_or(Ok(()), |mut state| state.close())
    }
}

fn closed() -> SQLError {
    SQLError::Internal("registered aggregate state is closed".into())
}

impl Drop for State {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub(super) struct BoundAggregate {
    pub(super) function: Arc<dyn SQLAggregateFactory>,
    pub(super) context: SQLFunctionContext,
}

impl SQLAggregateFactory for BoundAggregate {
    fn try_create_state(&self) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        self.function.try_create_state_with_context(&self.context)
    }
}
