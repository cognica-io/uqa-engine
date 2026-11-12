//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary wire and retained native registries share durable allocation restoration.

use super::{GraphIdentifierScope, GraphStoreError, GraphStoreResult, LabelKind};
use crate::memory_store::{NativeLabels, Registry};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{ProductionControl, ProductionVec},
    ordering::{sort_by_with_progress, SortProgress},
};
use uqa_storage::{catalog::graph_identifiers::GraphIdentifierNamespace, StorageBackendError};

impl GraphIdentifierScope<'_> {
    pub(crate) async fn restore_native_registry_from(
        &self,
        graph: &str,
        registry: &mut NativeLabels,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> GraphStoreResult<()> {
        self.restore_registry_with(graph, registry, control, input)
            .await
    }

    pub(super) async fn restore_registry_with(
        &self,
        graph: &str,
        registry: &mut impl Registry,
        control: &ProductionControl<'_>,
        work: &mut impl Work,
    ) -> GraphStoreResult<()> {
        work.checkpoint().await?;
        control.check().map_err(resource)?;
        if registry.allocation_id() == [0; 16] {
            registry.set_allocation_id(work.identity(&self.namespace, graph, control).await?);
        }
        if let Some(last) = self.watermark(&self.label_key(registry.allocation_id()))? {
            let next = last
                .checked_add(1)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    GraphStoreError::CorruptGraph("invalid label allocation watermark".into())
                })?;
            registry.set_next_label(registry.next_label().max(next));
        }
        // Ordinary labels() lists the two live defaults before user labels sorted by ID. Duplicate user IDs remain separate reads, and unrelated sequence fields are untouched.
        let mut labels = ProductionVec::new(*control);
        for (_, id) in registry.user_labels() {
            work.checkpoint().await?;
            labels.push_copy(id).map_err(resource)?;
        }
        sort_by_with_progress(labels.as_mut_slice(), &mut Order { work, control }).await?;
        for kind in [LabelKind::Vertex, LabelKind::Edge] {
            if !registry.dropped(kind.default_label_id()) {
                self.restore_sequence(registry, kind.default_label_id(), control, work)
                    .await?;
            }
        }
        for &id in labels.iter() {
            self.restore_sequence(registry, id, control, work).await?;
        }
        Ok(())
    }

    async fn restore_sequence(
        &self,
        registry: &mut impl Registry,
        id: u32,
        control: &ProductionControl<'_>,
        work: &mut impl Work,
    ) -> GraphStoreResult<()> {
        work.checkpoint().await?;
        control.check().map_err(resource)?;
        if let Some(value) = self.watermark(&self.sequence_key(registry.allocation_id(), id))? {
            let value = registry.sequence(id).unwrap_or(0).max(value);
            registry.put_sequence(id, value).map_err(Into::into)?;
        }
        Ok(())
    }
}

pub(super) trait Work: Send {
    fn checkpoint(&mut self) -> impl std::future::Future<Output = GraphStoreResult<()>> + Send;
    fn identity(
        &mut self,
        namespace: &GraphIdentifierNamespace,
        graph: &str,
        control: &ProductionControl<'_>,
    ) -> impl std::future::Future<Output = GraphStoreResult<[u8; 16]>> + Send;
}

impl Work for () {
    async fn checkpoint(&mut self) -> GraphStoreResult<()> {
        Ok(())
    }
    async fn identity(
        &mut self,
        namespace: &GraphIdentifierNamespace,
        graph: &str,
        _control: &ProductionControl<'_>,
    ) -> GraphStoreResult<[u8; 16]> {
        Ok(namespace.legacy_graph_identity(graph))
    }
}

impl Work for AsyncImageInput<'_> {
    async fn checkpoint(&mut self) -> GraphStoreResult<()> {
        AsyncImageInput::checkpoint(self).await.map_err(resource)
    }
    async fn identity(
        &mut self,
        namespace: &GraphIdentifierNamespace,
        graph: &str,
        control: &ProductionControl<'_>,
    ) -> GraphStoreResult<[u8; 16]> {
        Ok(namespace
            .legacy_graph_identity_from(graph, control, self)
            .await?)
    }
}

struct Order<'a, 'c, W> {
    work: &'a mut W,
    control: &'a ProductionControl<'c>,
}
impl<W: Work> SortProgress<u32, GraphStoreError> for Order<'_, '_, W> {
    async fn checkpoint(&mut self) -> GraphStoreResult<()> {
        self.work.checkpoint().await?;
        self.control.check().map_err(resource)
    }
    async fn compare(&mut self, left: &u32, right: &u32) -> GraphStoreResult<std::cmp::Ordering> {
        Ok(left.cmp(right))
    }
}

fn resource(error: impl Into<StorageBackendError>) -> GraphStoreError {
    error.into().into()
}

#[cfg(test)]
mod tests;
