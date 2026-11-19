//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline opening and retained restoration share selection while name rendering owns its work.

use std::{
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
    RelationIdentity,
};
use uqa_storage::StorageBackendResult;

pub(super) trait Work {
    fn checkpoint(&mut self) -> impl Future<Output = StorageBackendResult<()>> + Send;
    fn name(
        &mut self,
        relation: &RelationIdentity,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = StorageBackendResult<Produced<String>>> + Send;
}

impl Work for () {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(())
    }
    async fn name(
        &mut self,
        relation: &RelationIdentity,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<Produced<String>> {
        Ok(relation.qualified_name_with_control(control)?)
    }
}

impl Work for AsyncImageInput<'_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(AsyncImageInput::checkpoint(self).await?)
    }
    async fn name(
        &mut self,
        relation: &RelationIdentity,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<Produced<String>> {
        control.check()?;
        let name = relation.qualified_name_from(self).await?;
        control.check()?;
        Ok(name.into())
    }
}

pub(super) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result,
        Poll::Pending => unreachable!("inline watermark selection never suspends"),
    }
}
