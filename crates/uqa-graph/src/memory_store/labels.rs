//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native label definitions, sequences and tombstones share immutable ordered roots.

use super::{
    graphid::{
        registry::{self, Registry},
        FIRST_USER_LABEL_ID,
    },
    GraphLabelInfo, GraphLabelRegistry, GraphStoreResult, LabelKind, MemoryBudget, NativeMap,
    NativeSet,
};
use uqa_core::memory::MemoryError;

#[derive(Debug, Clone)]
pub(super) struct NativeLabels {
    pub(super) allocation_id: [u8; 16],
    pub(super) labels: NativeMap<String, u32>,
    pub(super) kinds: NativeMap<String, LabelKind>,
    pub(super) sequences: NativeMap<u32, u64>,
    pub(super) dropped_label_ids: NativeSet<u32>,
    pub(super) next_label_id: u32,
}

impl NativeLabels {
    pub(super) fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self {
            labels: self.labels.into_mutation_budget(memory),
            kinds: self.kinds.into_mutation_budget(memory),
            sequences: self.sequences.into_mutation_budget(memory),
            dropped_label_ids: self.dropped_label_ids.into_mutation_budget(memory),
            ..self
        }
    }

    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            allocation_id: [0; 16],
            labels: NativeMap::new(memory),
            kinds: NativeMap::new(memory),
            sequences: NativeMap::new(memory),
            dropped_label_ids: NativeSet::new(memory),
            next_label_id: FIRST_USER_LABEL_ID,
        }
    }

    pub(super) fn retained_bytes(&self) -> Result<usize, MemoryError> {
        [
            self.labels.retained_bytes()?,
            self.kinds.retained_bytes()?,
            self.sequences.retained_bytes()?,
            self.dropped_label_ids.retained_bytes()?,
        ]
        .into_iter()
        .try_fold(0_usize, |sum, bytes| {
            sum.checked_add(bytes).ok_or(MemoryError::SizeOverflow)
        })
    }

    pub(super) fn to_record(&self) -> GraphLabelRegistry {
        GraphLabelRegistry {
            allocation_id: self.allocation_id,
            labels: self
                .labels
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            kinds: self
                .kinds
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            sequences: self
                .sequences
                .iter()
                .map(|(key, value)| (*key, *value))
                .collect(),
            dropped_label_ids: self.dropped_label_ids.to_set(),
            next_label_id: self.next_label_id,
        }
    }

    pub(super) fn label_id(&mut self, label: &str, kind: LabelKind) -> GraphStoreResult<u32> {
        registry::label_id(self, label, kind)
    }
    pub(super) fn label_kind(&self, label: &str) -> Option<LabelKind> {
        registry::label_kind(self, label)
    }
    pub(super) fn register_label(
        &mut self,
        label: &str,
        kind: LabelKind,
    ) -> GraphStoreResult<Option<u32>> {
        registry::register_label(self, label, kind)
    }
    pub(super) fn remove_label(&mut self, label: &str) -> Result<Option<u32>, MemoryError> {
        registry::remove_label(self, label)
    }
    pub(super) fn labels(&self) -> Vec<GraphLabelInfo> {
        registry::labels(self)
    }
    pub(super) fn next_sequence(&mut self, label_id: u32) -> GraphStoreResult<u64> {
        registry::next_sequence(self, label_id)
    }
    pub(super) fn observe(
        &mut self,
        label: &str,
        id: u64,
        kind: LabelKind,
    ) -> Result<bool, MemoryError> {
        registry::observe(self, label, id, kind)
    }
    pub(super) fn merge(&mut self, other: &GraphLabelRegistry) -> Result<(), MemoryError> {
        registry::merge(self, other)
    }
}

impl Registry for NativeLabels {
    type Error = MemoryError;
    fn label(&self, name: &str) -> Option<u32> {
        self.labels.get(name).copied()
    }
    fn kind(&self, name: &str) -> Option<LabelKind> {
        self.kinds.get(name).copied()
    }
    fn sequence(&self, id: u32) -> Option<u64> {
        self.sequences.get(&id).copied()
    }
    fn dropped(&self, id: u32) -> bool {
        self.dropped_label_ids.contains(&id)
    }
    fn next_label(&self) -> u32 {
        self.next_label_id
    }
    fn set_next_label(&mut self, id: u32) {
        self.next_label_id = id;
    }
    fn user_labels(&self) -> impl Iterator<Item = (&str, u32)> {
        self.labels.iter().map(|(name, id)| (name.as_str(), *id))
    }
    fn put_label(&mut self, name: &str, id: u32) -> Result<(), MemoryError> {
        self.labels.insert_named(name, id, 0, 0)
    }
    fn put_kind(&mut self, name: &str, kind: LabelKind) -> Result<(), MemoryError> {
        self.kinds.insert_named(name, kind, 0, 0)
    }
    fn put_sequence(&mut self, id: u32, value: u64) -> Result<(), MemoryError> {
        self.sequences.insert(id, value, 0, 0)
    }
    fn erase_label(&mut self, name: &str) -> Result<Option<u32>, MemoryError> {
        let id = self.labels.get(name).copied();
        self.labels.remove(name)?;
        Ok(id)
    }
    fn erase_kind(&mut self, name: &str) -> Result<(), MemoryError> {
        self.kinds.remove(name)?;
        Ok(())
    }
    fn erase_sequence(&mut self, id: u32) -> Result<(), MemoryError> {
        self.sequences.remove(&id)?;
        Ok(())
    }
    fn put_dropped(&mut self, id: u32) -> Result<bool, MemoryError> {
        if self.dropped_label_ids.contains(&id) {
            return Ok(false);
        }
        self.dropped_label_ids.insert(id, 0)?;
        Ok(true)
    }
    fn erase_dropped(&mut self, id: u32) -> Result<bool, MemoryError> {
        self.dropped_label_ids.remove(&id)
    }
}
