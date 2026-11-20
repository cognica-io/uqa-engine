//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline and retained copies execute the same native allocation and traversal steps.

use crate::{image_io::ImageIoError, ValueRetentionError};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

/// Cooperatively schedule native value-copy steps without choosing their destination allowance.
pub trait Work: Send {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), ValueRetentionError>> + Send;
}

impl Work for () {
    async fn checkpoint(&mut self) -> Result<(), ValueRetentionError> {
        Ok(())
    }
}

impl From<ImageIoError> for ValueRetentionError {
    fn from(error: ImageIoError) -> Self {
        match error {
            ImageIoError::Memory(error) => Self::Memory(error),
            ImageIoError::Cancelled(error) => Self::Cancelled(error),
            error => Self::Malformed {
                kind: "value copy continuation",
                reason: error.to_string(),
            },
        }
    }
}

impl Work for crate::image_io::AsyncImageInput<'_> {
    async fn checkpoint(&mut self) -> Result<(), ValueRetentionError> {
        Ok(crate::image_io::AsyncImageInput::checkpoint(self).await?)
    }
}

pub(super) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("inline value copying has no suspended operation"),
    }
}
