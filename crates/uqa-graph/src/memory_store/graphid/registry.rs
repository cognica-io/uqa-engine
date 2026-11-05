//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Label identity rules shared by wire registries and native immutable storage.

use super::{
    graphid_label_id, graphid_sequence, GraphLabelInfo, GraphLabelRegistry, GraphStoreError,
    GraphStoreResult, LabelKind, FIRST_USER_LABEL_ID, MAX_GRAPHID_LABEL_ID, MAX_GRAPHID_SEQUENCE,
};
use crate::age_names::{EDGE_DEFAULT_LABEL_NAME, VERTEX_DEFAULT_LABEL_NAME};

mod wire;

pub(in crate::memory_store) trait Registry {
    type Error: Into<GraphStoreError>;
    fn label(&self, name: &str) -> Option<u32>;
    fn kind(&self, name: &str) -> Option<LabelKind>;
    fn sequence(&self, id: u32) -> Option<u64>;
    fn dropped(&self, id: u32) -> bool;
    fn next_label(&self) -> u32;
    fn set_next_label(&mut self, id: u32);
    fn user_labels(&self) -> impl Iterator<Item = (&str, u32)>;
    fn put_label(&mut self, name: &str, id: u32) -> Result<(), Self::Error>;
    fn put_kind(&mut self, name: &str, kind: LabelKind) -> Result<(), Self::Error>;
    fn put_sequence(&mut self, id: u32, value: u64) -> Result<(), Self::Error>;
    fn erase_label(&mut self, name: &str) -> Result<Option<u32>, Self::Error>;
    fn erase_kind(&mut self, name: &str) -> Result<(), Self::Error>;
    fn erase_sequence(&mut self, id: u32) -> Result<(), Self::Error>;
    fn put_dropped(&mut self, id: u32) -> Result<bool, Self::Error>;
    fn erase_dropped(&mut self, id: u32) -> Result<bool, Self::Error>;
}

pub(in crate::memory_store) fn label_id<R: Registry>(
    registry: &mut R,
    label: &str,
    kind: LabelKind,
) -> GraphStoreResult<u32> {
    if label.is_empty() {
        require_default(registry, kind)?;
        return Ok(kind.default_label_id());
    }
    for reserved in [LabelKind::Vertex, LabelKind::Edge] {
        if label == reserved.default_label_name() {
            require_kind(label, reserved, kind)?;
            require_default(registry, reserved)?;
            return Ok(reserved.default_label_id());
        }
    }
    if let Some(existing) = registry.kind(label) {
        require_kind(label, existing, kind)?;
    }
    if let Some(id) = registry.label(label) {
        if id > MAX_GRAPHID_LABEL_ID {
            return Err(GraphStoreError::IdExhausted(format!(
                "persisted label id {id} exceeds {MAX_GRAPHID_LABEL_ID}"
            )));
        }
        if registry.kind(label).is_none() {
            registry.put_kind(label, kind).map_err(Into::into)?;
        }
        return Ok(id);
    }
    require_default(registry, kind)?;
    let id = allocate_label(registry)?;
    registry.put_label(label, id).map_err(Into::into)?;
    registry.put_kind(label, kind).map_err(Into::into)?;
    Ok(id)
}

fn require_default(registry: &impl Registry, kind: LabelKind) -> GraphStoreResult<()> {
    if registry.dropped(kind.default_label_id()) {
        return Err(GraphStoreError::InvalidMutation(format!(
            "default label {} does not exist",
            kind.default_label_name()
        )));
    }
    Ok(())
}

fn require_kind(label: &str, existing: LabelKind, requested: LabelKind) -> GraphStoreResult<()> {
    if existing == requested {
        return Ok(());
    }
    Err(GraphStoreError::InvalidMutation(format!(
        "label {label} is for {}, not {}",
        existing.entity_noun(),
        requested.entity_noun()
    )))
}

fn allocate_label(registry: &mut impl Registry) -> GraphStoreResult<u32> {
    let id = registry.next_label();
    if id > MAX_GRAPHID_LABEL_ID {
        return Err(GraphStoreError::IdExhausted(format!(
            "label id {id} exceeds {MAX_GRAPHID_LABEL_ID}"
        )));
    }
    registry.set_next_label(
        id.checked_add(1)
            .ok_or_else(|| GraphStoreError::IdExhausted("label id counter overflow".into()))?,
    );
    Ok(id)
}

pub(in crate::memory_store) fn contains_label(registry: &impl Registry, label: &str) -> bool {
    for kind in [LabelKind::Vertex, LabelKind::Edge] {
        if label == kind.default_label_name() {
            return !registry.dropped(kind.default_label_id());
        }
    }
    registry.label(label).is_some()
}

pub(in crate::memory_store) fn label_kind(
    registry: &impl Registry,
    label: &str,
) -> Option<LabelKind> {
    for kind in [LabelKind::Vertex, LabelKind::Edge] {
        if label == kind.default_label_name() {
            return contains_label(registry, label).then_some(kind);
        }
    }
    registry.label(label)?;
    Some(registry.kind(label).unwrap_or(LabelKind::Vertex))
}

pub(in crate::memory_store) fn register_label<R: Registry>(
    registry: &mut R,
    label: &str,
    kind: LabelKind,
) -> GraphStoreResult<Option<u32>> {
    if contains_label(registry, label) {
        return Ok(None);
    }
    require_default(registry, kind)?;
    if label == VERTEX_DEFAULT_LABEL_NAME || label == EDGE_DEFAULT_LABEL_NAME {
        return Err(GraphStoreError::InvalidMutation(format!(
            "default label {label} cannot be recreated without recreating the graph"
        )));
    }
    let id = allocate_label(registry)?;
    registry.put_label(label, id).map_err(Into::into)?;
    registry.put_kind(label, kind).map_err(Into::into)?;
    Ok(Some(id))
}

pub(in crate::memory_store) fn remove_label<R: Registry>(
    registry: &mut R,
    label: &str,
) -> Result<Option<u32>, R::Error> {
    for kind in [LabelKind::Vertex, LabelKind::Edge] {
        if label == kind.default_label_name() {
            if !registry.put_dropped(kind.default_label_id())? {
                return Ok(None);
            }
            registry.erase_sequence(kind.default_label_id())?;
            return Ok(Some(kind.default_label_id()));
        }
    }
    let Some(id) = registry.erase_label(label)? else {
        return Ok(None);
    };
    registry.erase_kind(label)?;
    registry.erase_sequence(id)?;
    registry.put_dropped(id)?;
    Ok(Some(id))
}

pub(in crate::memory_store) fn labels(registry: &impl Registry) -> Vec<GraphLabelInfo> {
    let mut out = Vec::new();
    for kind in [LabelKind::Vertex, LabelKind::Edge] {
        if !registry.dropped(kind.default_label_id()) {
            out.push(GraphLabelInfo {
                name: kind.default_label_name().into(),
                id: kind.default_label_id(),
                kind,
                last_sequence: registry.sequence(kind.default_label_id()).unwrap_or(0),
            });
        }
    }
    let mut user: Vec<_> = registry
        .user_labels()
        .map(|(name, id)| GraphLabelInfo {
            name: name.into(),
            id,
            kind: label_kind(registry, name).unwrap_or(LabelKind::Vertex),
            last_sequence: registry.sequence(id).unwrap_or(0),
        })
        .collect();
    user.sort_by_key(|label| label.id);
    out.extend(user);
    out
}

pub(in crate::memory_store) fn next_sequence<R: Registry>(
    registry: &mut R,
    label_id: u32,
) -> GraphStoreResult<u64> {
    let current = registry.sequence(label_id).unwrap_or(0);
    let next = current.checked_add(1).ok_or_else(|| {
        GraphStoreError::IdExhausted(format!("sequence counter overflow for label id {label_id}"))
    })?;
    if next > MAX_GRAPHID_SEQUENCE {
        return Err(GraphStoreError::IdExhausted(format!(
            "sequence {next} exceeds {MAX_GRAPHID_SEQUENCE} for label id {label_id}"
        )));
    }
    registry.put_sequence(label_id, next).map_err(Into::into)?;
    Ok(next)
}

pub(in crate::memory_store) fn observe<R: Registry>(
    registry: &mut R,
    label: &str,
    id: u64,
    kind: LabelKind,
) -> Result<bool, R::Error> {
    let label_id = graphid_label_id(id);
    if label_id == 0 {
        return Ok(false);
    }
    let mut changed = false;
    if !label.is_empty() && label_id >= FIRST_USER_LABEL_ID {
        if registry.label(label).is_none() {
            registry.put_label(label, label_id)?;
            changed = true;
        }
        if registry.kind(label).is_none() {
            registry.put_kind(label, kind)?;
            changed = true;
        }
    }
    changed |= registry.erase_dropped(label_id)?;
    let sequence = graphid_sequence(id);
    if registry
        .sequence(label_id)
        .is_none_or(|current| sequence > current)
    {
        registry.put_sequence(label_id, sequence)?;
        changed = true;
    }
    if label_id >= registry.next_label() {
        registry.set_next_label(label_id + 1);
        changed = true;
    }
    Ok(changed)
}

pub(in crate::memory_store) fn merge<R: Registry>(
    registry: &mut R,
    other: &GraphLabelRegistry,
) -> Result<(), R::Error> {
    for (name, id) in &other.labels {
        if registry.label(name).is_none() {
            registry.put_label(name, *id)?;
        }
    }
    for (name, kind) in &other.kinds {
        if registry.kind(name).is_none() {
            registry.put_kind(name, *kind)?;
        }
    }
    for (id, sequence) in &other.sequences {
        if registry
            .sequence(*id)
            .is_none_or(|current| *sequence > current)
        {
            registry.put_sequence(*id, *sequence)?;
        }
    }
    for id in &other.dropped_label_ids {
        registry.put_dropped(*id)?;
    }
    if other.next_label_id > registry.next_label() {
        registry.set_next_label(other.next_label_id);
    }
    Ok(())
}
