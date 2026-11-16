//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projected syntax retains its typed constructor and result through the shared native SQL driver.

use super::{frame, Input, JsonReadError, JsonReadWork, Native, Projected, Read, Result};
use crate::construction;

type Output<T> = construction::Output<Result<Native<T>>>;
pub(in crate::ast::query_input) type Constructor<'i, A, T> =
    for<'a> fn(Input<'i>, A, &'a mut Work<'i>) -> Result<Read<'a, T>>;

#[derive(Clone)]
pub(in crate::ast::query_input) struct Work<'a> {
    inner: construction::Work<'a>,
}

impl<'a> Work<'a> {
    pub(super) fn submit<T: Projected>(&mut self, input: Input<'a>) -> Result<Output<T>> {
        self.submit_with(input, (), projected::<T>)
    }

    fn submit_with<T: Send + 'static, A: Send + 'a>(
        &mut self,
        input: Input<'a>,
        argument: A,
        constructor: Constructor<'a, A, T>,
    ) -> Result<Output<T>> {
        Ok(self
            .inner
            .submit(move |inner| async move {
                let mut work = Self { inner };
                let result = match constructor(input, argument, &mut work) {
                    Ok(future) => future.await,
                    Err(error) => Err(error),
                };
                result
            })
            .map_err(JsonReadError::from)?)
    }
}

fn projected<'i, 'a, T: Projected>(
    input: Input<'i>,
    (): (),
    work: &'a mut Work<'i>,
) -> Result<Read<'a, T>> {
    if T::BUFFERED && !input.buffered {
        frame(input.control, async move {
            let prepared = input.buffer(work).await?;
            let result = input
                .buffered_value(prepared.as_ref())
                .run::<T, _>(work)?
                .await;
            result
        })
    } else {
        T::read_value(input, work)
    }
}

impl JsonReadWork for Work<'_> {
    type Error = JsonReadError;

    async fn checkpoint(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.checkpoint().await;
        Ok(())
    }
}

pub(super) fn run<'a, 'w, T: Projected, W: JsonReadWork<Error = JsonReadError> + 'w>(
    input: Input<'a>,
    work: &'w mut W,
) -> Result<Read<'w, T>>
where
    'a: 'w,
{
    run_with(input, (), work, projected::<T>)
}

pub(in crate::ast::query_input) fn run_with<
    'a,
    'w,
    T: Send + 'static,
    A: Send + 'a,
    W: JsonReadWork<Error = JsonReadError> + 'w,
>(
    input: Input<'a>,
    argument: A,
    work: &'w mut W,
    constructor: Constructor<'a, A, T>,
) -> Result<Read<'w, T>>
where
    'a: 'w,
{
    frame(input.control, async move {
        let mut driver = construction::Driver::new(input.control).map_err(JsonReadError::from)?;
        let mut owner = Work {
            inner: driver.work(),
        };
        let output = owner.submit_with(input, argument, constructor)?;
        while driver.advance().await.map_err(JsonReadError::from)? {
            work.checkpoint().await?;
        }
        construction::receive(output).await
    })
}

pub(super) async fn receive<T>(output: Output<T>) -> Result<Native<T>> {
    construction::receive(output).await
}
