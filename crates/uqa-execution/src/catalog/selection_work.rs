//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed catalog selection shares exact comparisons between inline and retained work.

use std::{
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};
use uqa_sql::SQLError;

pub(crate) trait Work: Send {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), SQLError>> + Send;
}
impl Work for () {
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Ok(())
    }
}
impl Work for AsyncImageInput<'_> {
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Ok(AsyncImageInput::checkpoint(self).await?)
    }
}

pub(crate) async fn checkpoint(
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    Ok(())
}

pub(crate) async fn equal(
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<bool, SQLError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        checkpoint(control, work).await?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("inline catalog selection never suspends"),
    }
}
