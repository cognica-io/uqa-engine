//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing public and serialized registry fields use the shared identity rules.

use super::{GraphLabelRegistry, GraphStoreError, LabelKind, Registry};
use std::convert::Infallible;

impl From<Infallible> for GraphStoreError {
    fn from(error: Infallible) -> Self {
        match error {}
    }
}

impl Registry for GraphLabelRegistry {
    type Error = Infallible;
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
    fn put_label(&mut self, name: &str, id: u32) -> Result<(), Infallible> {
        self.labels.insert(name.into(), id);
        Ok(())
    }
    fn put_kind(&mut self, name: &str, kind: LabelKind) -> Result<(), Infallible> {
        self.kinds.insert(name.into(), kind);
        Ok(())
    }
    fn put_sequence(&mut self, id: u32, value: u64) -> Result<(), Infallible> {
        self.sequences.insert(id, value);
        Ok(())
    }
    fn erase_label(&mut self, name: &str) -> Result<Option<u32>, Infallible> {
        Ok(self.labels.remove(name))
    }
    fn erase_kind(&mut self, name: &str) -> Result<(), Infallible> {
        self.kinds.remove(name);
        Ok(())
    }
    fn erase_sequence(&mut self, id: u32) -> Result<(), Infallible> {
        self.sequences.remove(&id);
        Ok(())
    }
    fn put_dropped(&mut self, id: u32) -> Result<bool, Infallible> {
        Ok(self.dropped_label_ids.insert(id))
    }
    fn erase_dropped(&mut self, id: u32) -> Result<bool, Infallible> {
        Ok(self.dropped_label_ids.remove(&id))
    }
}
