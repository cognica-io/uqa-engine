//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native value constructors share their steps between inline calls and an enclosing input scheduler.

use crate::{
    json::{JsonReadError, JsonReadWork},
    ValueRetentionError,
};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

pub(crate) trait Work: Send {
    type Error: From<ValueRetentionError>;
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

impl Work for () {
    type Error = ValueRetentionError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) struct JsonWork<'a, W>(pub &'a mut W);

pub(crate) enum JsonWorkError {
    Work(JsonReadError),
    Native(ValueRetentionError),
}

impl From<ValueRetentionError> for JsonWorkError {
    fn from(error: ValueRetentionError) -> Self {
        Self::Native(error)
    }
}

impl<W: JsonReadWork<Error = JsonReadError>> Work for JsonWork<'_, W> {
    type Error = JsonWorkError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        self.0.checkpoint().await.map_err(JsonWorkError::Work)
    }
}

pub(crate) enum Stack<T> {
    Ordinary(Vec<T>),
    Admitted(crate::memory::BudgetedVec<T>),
}

impl<T> Stack<T> {
    pub(crate) fn new(budget: Option<&crate::memory::MemoryBudget>) -> Self {
        budget.map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Admitted(crate::memory::BudgetedVec::new(budget)),
        )
    }
    pub(crate) fn push(&mut self, value: T) -> Result<(), crate::memory::MemoryError> {
        match self {
            Self::Ordinary(values) => {
                values.try_reserve(1)?;
                values.push(value);
                Ok(())
            }
            Self::Admitted(values) => values.push(value),
        }
    }
    pub(crate) fn last_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Ordinary(values) => values.last_mut(),
            Self::Admitted(values) => values.last_mut(),
        }
    }
    pub(crate) fn pop(&mut self) -> Option<T> {
        match self {
            Self::Ordinary(values) => values.pop(),
            Self::Admitted(values) => values.pop(),
        }
    }
}

pub(crate) async fn checkpoint<W: Work>(
    work: &mut W,
    check: &mut (impl FnMut() -> Result<(), ValueRetentionError> + Send),
) -> Result<(), W::Error> {
    check()?;
    work.checkpoint().await?;
    check()?;
    Ok(())
}

pub(crate) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("inline value construction has no suspended operation"),
    }
}
