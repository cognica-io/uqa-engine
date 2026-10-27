//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registered aggregate resource ownership, independent of value finalization.

use super::{AggregateAccumulator, SQLAggregateFactory, SQLAggregateState, SQLError, Value};

#[derive(Default)]
pub(super) struct RegisteredAggregateState(Option<Box<dyn SQLAggregateState>>);

impl RegisteredAggregateState {
    pub(super) fn new(state: Box<dyn SQLAggregateState>) -> Self {
        Self(Some(state))
    }

    pub(super) fn replay(&self, factory: &dyn SQLAggregateFactory) -> Result<Self, SQLError> {
        match &self.0 {
            Some(_) => factory.try_create_state().map(Self::new),
            None => Err(SQLError::Internal(
                "registered aggregate state is closed".into(),
            )),
        }
    }
}

impl SQLAggregateState for RegisteredAggregateState {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        self.0
            .as_mut()
            .ok_or_else(|| SQLError::Internal("registered aggregate state is closed".into()))?
            .observe(args)
    }

    fn finish(&self) -> Result<Value, SQLError> {
        self.0
            .as_ref()
            .ok_or_else(|| SQLError::Internal("registered aggregate state is closed".into()))?
            .finish()
    }

    fn close(&mut self) -> Result<(), SQLError> {
        // Transfer ownership before calling user code: a failed close is final.
        self.0.take().map_or(Ok(()), |mut state| state.close())
    }
}

impl Drop for RegisteredAggregateState {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub(crate) fn with_cleanup<T>(
    execution: Result<T, SQLError>,
    cleanup: Result<(), SQLError>,
) -> Result<T, SQLError> {
    match (execution, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(error.with_cleanup(cleanup, "close aggregate state")),
    }
}

pub(super) fn close_accumulators(
    accumulators: &mut [AggregateAccumulator],
) -> Result<(), SQLError> {
    let mut result = Ok(());
    for accumulator in accumulators {
        result = with_cleanup(result, accumulator.close());
    }
    result
}
