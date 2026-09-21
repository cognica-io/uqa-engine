//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable entity reservations are shared by every graph using the same physical identity space. Label definitions remain transactional.

use std::num::NonZeroU64;

use uqa_storage::catalog::graph_identifiers::GraphIdentifierNamespace;
use uqa_storage::mvcc::{IdentifierAllocator, IdentifierRequest};
use uqa_storage::{GraphEntityFilter, GraphEntityKind};

use crate::memory_store::{MAX_GRAPHID_LABEL_ID, MAX_GRAPHID_SEQUENCE as MAX_SEQUENCE};
use crate::{
    graphid_label_id, graphid_sequence, GraphLabelRegistry, GraphStoreError, GraphStoreResult,
    LabelKind, GRAPHID_LABEL_SHIFT,
};

use super::{PersistentGraphStore, ID_PAGE_SIZE};

/// A database-local physical graph namespace. Catalog graphs share one namespace; standalone families use their canonical provider scope. A transactional clear selects a new generation.
pub struct GraphIdentifierScope<'a> {
    allocator: &'a dyn IdentifierAllocator,
    namespace: GraphIdentifierNamespace,
}

pub fn decode_identifier_generation(value: Option<&str>) -> GraphStoreResult<[u8; 16]> {
    value.map_or(Ok([0; 16]), |value| {
        serde_json::from_str(value)
            .map_err(|error| GraphStoreError::CorruptGraph(error.to_string()))
    })
}

impl<'a> GraphIdentifierScope<'a> {
    pub(super) fn namespace(&self) -> GraphIdentifierNamespace {
        self.namespace
    }
    pub fn catalog(allocator: &'a dyn IdentifierAllocator, generation: [u8; 16]) -> Self {
        Self::new(allocator, None, generation)
    }

    pub fn standalone(
        allocator: &'a dyn IdentifierAllocator,
        scope: &str,
        generation: [u8; 16],
    ) -> Self {
        Self::new(allocator, Some(scope), generation)
    }

    fn new(
        allocator: &'a dyn IdentifierAllocator,
        scope: Option<&str>,
        generation: [u8; 16],
    ) -> Self {
        Self {
            allocator,
            namespace: GraphIdentifierNamespace::new(scope, generation),
        }
    }

    fn legacy_graph_identity(&self, graph: &str) -> [u8; 16] {
        self.namespace.legacy_graph_identity(graph)
    }

    fn watermark(&self, key: &[u8]) -> GraphStoreResult<Option<u64>> {
        Ok(self.allocator.identifier_watermark(key)?)
    }

    fn observe(&self, key: &[u8], value: u64) -> GraphStoreResult<()> {
        if self.watermark(key)?.is_none_or(|current| current < value) {
            self.allocator
                .allocate_identifiers(key, IdentifierRequest::Observe(value))?;
        }
        Ok(())
    }

    fn reserve(&self, key: &[u8], minimum: u64, maximum: u64) -> GraphStoreResult<u64> {
        if minimum > maximum {
            return Err(GraphStoreError::IdExhausted(
                "graph identifier space is exhausted".into(),
            ));
        }
        Ok(self
            .allocator
            .allocate_identifiers(
                key,
                IdentifierRequest::Reserve {
                    minimum,
                    maximum,
                    count: NonZeroU64::MIN,
                },
            )?
            .watermark())
    }

    fn entity_key(&self, kind: GraphEntityKind, prefix: u32) -> [u8; 78] {
        self.namespace.entity_key(kind, prefix)
    }

    fn hint_key(&self, kind: GraphEntityKind) -> [u8; 78] {
        self.namespace.hint_key(kind)
    }

    fn sequence_key(&self, graph: [u8; 16], prefix: u32) -> [u8; 78] {
        self.namespace.sequence_key(graph, prefix)
    }

    fn label_key(&self, graph: [u8; 16]) -> [u8; 78] {
        self.namespace.label_key(graph)
    }

    pub(super) fn restore_registry(
        &self,
        graph: &str,
        registry: &mut GraphLabelRegistry,
    ) -> GraphStoreResult<()> {
        if registry.allocation_id == [0; 16] {
            registry.allocation_id = self.legacy_graph_identity(graph);
        }
        if let Some(last) = self.watermark(&self.label_key(registry.allocation_id))? {
            let next = last
                .checked_add(1)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    GraphStoreError::CorruptGraph("invalid label allocation watermark".into())
                })?;
            registry.next_label_id = registry.next_label_id.max(next);
        }
        for label in registry.labels() {
            let key = self.sequence_key(registry.allocation_id, label.id);
            if let Some(value) = self.watermark(&key)? {
                registry
                    .sequences
                    .entry(label.id)
                    .and_modify(|old| *old = (*old).max(value))
                    .or_insert(value);
            }
        }
        Ok(())
    }
}

fn entity_kind(kind: LabelKind) -> GraphEntityKind {
    match kind {
        LabelKind::Vertex => GraphEntityKind::Vertex,
        LabelKind::Edge => GraphEntityKind::Edge,
    }
}

impl PersistentGraphStore {
    /// Seed only an uninitialized physical prefix. Identity pages omit payloads and retain a fixed upper bound on workspace.
    fn prefix_floor(
        &self,
        identifiers: &GraphIdentifierScope<'_>,
        kind: GraphEntityKind,
        prefix: u32,
    ) -> GraphStoreResult<u64> {
        let observed = identifiers.watermark(&identifiers.entity_key(kind, prefix))?;
        if identifiers
            .watermark(&identifiers.namespace.entity_seed_key(kind, prefix))?
            .is_some()
        {
            return Ok(observed.unwrap_or(0));
        }
        let mut floor = self
            .storage
            .counter(kind)?
            .and_then(|next| next.checked_sub(1))
            .filter(|id| graphid_label_id(*id) == prefix)
            .map_or(0, graphid_sequence)
            .max(observed.unwrap_or(0));
        let mut after = (u64::from(prefix) << GRAPHID_LABEL_SHIFT).checked_sub(1);
        'pages: loop {
            let ids = self
                .storage
                .ids(GraphEntityFilter::new(kind, None), after, ID_PAGE_SIZE)?;
            if ids.is_empty() {
                break;
            }
            after = ids.last().copied();
            for id in ids {
                if graphid_label_id(id) != prefix {
                    break 'pages;
                }
                floor = floor.max(graphid_sequence(id));
            }
        }
        for graph in self.storage.graph_names()? {
            let registry = self.storage.registry(&graph)?;
            for label in registry.labels() {
                if label.id == prefix && entity_kind(label.kind) == kind {
                    floor = floor.max(label.last_sequence);
                }
            }
        }
        identifiers.observe(&identifiers.entity_key(kind, prefix), floor)?;
        identifiers.observe(&identifiers.namespace.entity_seed_key(kind, prefix), 1)?;
        Ok(floor)
    }

    pub(super) fn observe_durable_id(
        &self,
        identifiers: &GraphIdentifierScope<'_>,
        kind: GraphEntityKind,
        id: u64,
    ) -> GraphStoreResult<()> {
        let prefix = graphid_label_id(id);
        let floor = self.prefix_floor(identifiers, kind, prefix)?;
        identifiers.observe(
            &identifiers.entity_key(kind, prefix),
            floor.max(graphid_sequence(id)),
        )?;
        identifiers.observe(&identifiers.hint_key(kind), id)
    }

    pub(super) fn allocate_durable_counter(
        &self,
        identifiers: &GraphIdentifierScope<'_>,
        kind: GraphEntityKind,
    ) -> GraphStoreResult<u64> {
        let mut minimum = self.next_counter(kind)?;
        if let Some(last) = identifiers.watermark(&identifiers.hint_key(kind))? {
            minimum =
                minimum.max(last.checked_add(1).ok_or_else(|| {
                    GraphStoreError::IdExhausted("graph id counter overflow".into())
                })?);
        }
        loop {
            let prefix = graphid_label_id(minimum);
            let floor = self.prefix_floor(identifiers, kind, prefix)?;
            if floor > MAX_SEQUENCE {
                return Err(GraphStoreError::CorruptGraph(
                    "invalid graph entity allocation watermark".into(),
                ));
            }
            if floor == MAX_SEQUENCE {
                minimum = (u64::from(prefix) + 1) << GRAPHID_LABEL_SHIFT;
                if prefix == u32::from(u16::MAX) {
                    return Err(GraphStoreError::IdExhausted(
                        "graph id counter overflow".into(),
                    ));
                }
                continue;
            }
            let sequence = identifiers.reserve(
                &identifiers.entity_key(kind, prefix),
                graphid_sequence(minimum).max(floor + 1),
                MAX_SEQUENCE,
            )?;
            let id = (u64::from(prefix) << GRAPHID_LABEL_SHIFT) | sequence;
            if id == u64::MAX {
                return Err(GraphStoreError::IdExhausted(
                    "graph id counter overflow".into(),
                ));
            }
            identifiers.observe(&identifiers.hint_key(kind), id)?;
            return Ok(id);
        }
    }

    pub(super) fn allocate_durable_label(
        &self,
        identifiers: &GraphIdentifierScope<'_>,
        registry: &GraphLabelRegistry,
        label: u32,
        kind: LabelKind,
    ) -> GraphStoreResult<u64> {
        let kind = entity_kind(kind);
        let floor = self
            .prefix_floor(identifiers, kind, label)?
            .max(registry.sequences.get(&label).copied().unwrap_or(0));
        let minimum = floor
            .checked_add(1)
            .ok_or_else(|| GraphStoreError::IdExhausted("graph sequence overflow".into()))?;
        let sequence =
            identifiers.reserve(&identifiers.entity_key(kind, label), minimum, MAX_SEQUENCE)?;
        let id = crate::make_graphid(label, sequence)?;
        identifiers.observe(
            &identifiers.sequence_key(registry.allocation_id, label),
            sequence,
        )?;
        identifiers.observe(&identifiers.hint_key(kind), id)?;
        Ok(id)
    }

    pub(super) fn reserve_label_definition(
        &self,
        registry: &mut GraphLabelRegistry,
        label: &str,
        proposed: u32,
    ) -> GraphStoreResult<u32> {
        let Some(identifiers) = self.storage.identifiers()? else {
            return Ok(proposed);
        };
        let allocated = identifiers.reserve(
            &identifiers.label_key(registry.allocation_id),
            u64::from(proposed),
            u64::from(MAX_GRAPHID_LABEL_ID),
        )?;
        let allocated = u32::try_from(allocated)
            .map_err(|_| GraphStoreError::IdExhausted("graph label id overflow".into()))?;
        registry.labels.insert(label.to_owned(), allocated);
        registry.next_label_id = allocated + 1;
        Ok(allocated)
    }

    pub(super) fn resolve_label(
        &self,
        registry: &mut GraphLabelRegistry,
        label: &str,
        kind: LabelKind,
    ) -> GraphStoreResult<u32> {
        let new = !label.is_empty()
            && label != kind.default_label_name()
            && !registry.labels.contains_key(label);
        let proposed = registry.label_id(label, kind)?;
        if new {
            self.reserve_label_definition(registry, label, proposed)
        } else {
            Ok(proposed)
        }
    }

    pub(super) fn save_registry(
        &self,
        graph: &str,
        registry: &GraphLabelRegistry,
    ) -> GraphStoreResult<()> {
        if let Some(identifiers) = self.storage.identifiers()? {
            for label in registry.labels() {
                if label.last_sequence != 0 {
                    let kind = entity_kind(label.kind);
                    identifiers.observe(
                        &identifiers.sequence_key(registry.allocation_id, label.id),
                        label.last_sequence,
                    )?;
                    let id = crate::make_graphid(label.id, label.last_sequence)?;
                    self.observe_durable_id(&identifiers, kind, id)?;
                }
            }
            identifiers.observe(
                &identifiers.label_key(registry.allocation_id),
                u64::from(registry.next_label_id.saturating_sub(1)),
            )?;
            let previous = self.storage.registry(graph)?;
            let previous_identity = if previous.allocation_id == [0; 16] {
                identifiers.legacy_graph_identity(graph)
            } else {
                previous.allocation_id
            };
            if previous_identity == registry.allocation_id
                && previous.labels == registry.labels
                && previous.kinds == registry.kinds
                && previous.dropped_label_ids == registry.dropped_label_ids
            {
                return Ok(());
            }
        }
        self.storage.save_registry(graph, registry)
    }
}
