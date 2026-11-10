//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native analysis constructors share ordinary and retained scheduling.

use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

pub(crate) trait Work<E>: Send {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), E>> + Send;

    fn records(&mut self, index: usize) -> impl Future<Output = Result<(), E>> + Send {
        async move {
            if index.is_multiple_of(128) {
                self.checkpoint().await?;
            }
            Ok(())
        }
    }
}

impl<E> Work<E> for () {
    async fn checkpoint(&mut self) -> Result<(), E> {
        Ok(())
    }
}

impl<E: From<uqa_core::image_io::ImageIoError>> Work<E>
    for uqa_core::image_io::AsyncImageInput<'_>
{
    async fn checkpoint(&mut self) -> Result<(), E> {
        uqa_core::image_io::AsyncImageInput::checkpoint(self)
            .await
            .map_err(Into::into)
    }
}

pub(crate) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("ordinary analysis construction has no suspended work"),
    }
}
