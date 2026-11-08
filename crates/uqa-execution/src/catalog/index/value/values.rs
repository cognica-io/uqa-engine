//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated keys keep immutable native roots and cached independent capture footprints.

use uqa_core::{
    memory::{
        Budgeted, MemoryBudget, MemoryError, RetainedMap, RetainedMapIter, RetainedMapSnapshot,
    },
    CancellationToken, DocId, Value,
};

/// Ordinary column accelerators retain their existing infallible native-allocation API. Their node owner is unbounded; cache spill decisions and invoking read/capture allowances remain separate. Payload accounting observes physical carriers without SQL comparison or type interpretation.
#[derive(Clone)]
pub(super) struct StoredValues {
    root: RetainedMap<DocId, Value>,
    cancellation: CancellationToken,
}

impl StoredValues {
    pub(super) fn new() -> Self {
        Self {
            root: RetainedMap::new(&MemoryBudget::new(usize::MAX)),
            cancellation: CancellationToken::new(),
        }
    }

    pub(super) fn get(&self, id: DocId) -> Option<&Value> {
        self.root.get(&id)
    }

    pub(super) fn contains_key(&self, id: DocId) -> bool {
        self.root.get(&id).is_some()
    }

    pub(super) fn insert(&mut self, id: DocId, value: Value) {
        let bytes = value
            .retained_payload_bytes(self.root.budget(), &self.cancellation)
            .expect("addressable native evaluated-key payload");
        self.root
            .insert(id, value, bytes)
            .expect("addressable native evaluated-key root");
    }

    pub(super) fn remove(&mut self, id: DocId) {
        self.root
            .remove(&id)
            .expect("addressable native evaluated-key path");
    }

    pub(super) fn clear(&mut self) {
        self.root.clear();
    }

    pub(super) fn capture(
        &self,
        memory: &MemoryBudget,
    ) -> Result<Budgeted<RetainedMapSnapshot<DocId, Value>>, MemoryError> {
        self.root.capture(memory)
    }
}

impl<'a> IntoIterator for &'a StoredValues {
    type Item = (&'a DocId, &'a Value);
    type IntoIter = RetainedMapIter<'a, DocId, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.root.iter()
    }
}

impl FromIterator<(DocId, Value)> for StoredValues {
    fn from_iter<T: IntoIterator<Item = (DocId, Value)>>(values: T) -> Self {
        let mut root = Self::new();
        for (id, value) in values {
            root.insert(id, value);
        }
        root
    }
}
