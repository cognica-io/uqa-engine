//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical name rendering shares its construction loop between ordinary and retained callers.

use crate::{image_io::ImageIoError, ValueRetentionError};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

pub(super) trait Work: Send {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), ValueRetentionError>> + Send;
}

impl Work for () {
    async fn checkpoint(&mut self) -> Result<(), ValueRetentionError> {
        Ok(())
    }
}

impl Work for crate::image_io::AsyncImageInput<'_> {
    async fn checkpoint(&mut self) -> Result<(), ValueRetentionError> {
        crate::image_io::AsyncImageInput::checkpoint(self)
            .await
            .map_err(|error| match error {
                ImageIoError::Memory(error) => ValueRetentionError::Memory(error),
                ImageIoError::Cancelled(error) => ValueRetentionError::Cancelled(error),
                error => ValueRetentionError::Malformed {
                    kind: "relation name rendering",
                    reason: error.to_string(),
                },
            })
    }
}

pub(super) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("ordinary relation rendering has no suspended work"),
    }
}
