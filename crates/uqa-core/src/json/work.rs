//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Both drivers execute one grammar; inline checkpoints are immediately ready.

use super::JsonReadError;
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

/// Schedule borrowed JSON construction while preserving the enclosing operation's errors.
pub trait Work: Send {
    type Error: From<JsonReadError>;

    fn checkpoint(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

impl Work for () {
    type Error = JsonReadError;

    async fn checkpoint(&mut self) -> Result<(), JsonReadError> {
        Ok(())
    }
}

impl Work for crate::image_io::AsyncImageInput<'_> {
    type Error = JsonReadError;

    async fn checkpoint(&mut self) -> Result<(), JsonReadError> {
        crate::image_io::AsyncImageInput::checkpoint(self)
            .await
            .map_err(Into::into)
    }
}

pub(super) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("inline JSON work has no suspended operation"),
    }
}
