//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native transformations share their loops between inline and retained input.

use crate::{StorageBackendError, StorageBackendResult};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

pub(crate) trait Work<E = StorageBackendError> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), E>> + Send;
}

impl<E> Work<E> for () {
    async fn checkpoint(&mut self) -> Result<(), E> {
        Ok(())
    }
}

impl Work for uqa_core::image_io::AsyncImageInput<'_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(uqa_core::image_io::AsyncImageInput::checkpoint(self).await?)
    }
}

pub(crate) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("inline image work never suspends"),
    }
}
