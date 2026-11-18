//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native graph transformations share ordinary and retained work boundaries.

use std::{
    borrow::Borrow,
    cmp::Ordering,
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MapKeyInput, Produced},
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub(crate) struct CatalogNames<'a, 'input>(pub &'a mut AsyncImageInput<'input>);

impl<Q: Borrow<str> + Sync + ?Sized> MapKeyInput<Q, Produced<String>> for CatalogNames<'_, '_> {
    type Error = StorageBackendError;

    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.0.checkpoint().await?)
    }

    async fn compare(
        &mut self,
        query: &Q,
        stored: &Produced<String>,
    ) -> StorageBackendResult<Ordering> {
        uqa_storage::catalog::compare_names(query.borrow(), stored, self.0).await
    }
}

pub(crate) trait Work<E>: Send {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), E>> + Send;
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
        Poll::Pending => unreachable!("ordinary graph work has no suspended operation"),
    }
}
#[cfg(test)]
pub(crate) mod tests;
