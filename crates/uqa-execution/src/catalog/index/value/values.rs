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
    root: Values,
    cancellation: CancellationToken,
}

#[derive(Clone)]
enum Values {
    Native(RetainedMap<DocId, Value>),
    Admitted(RetainedMap<DocId, Budgeted<Value>>),
}

pub(super) enum StoredValuesSnapshot {
    Native(Budgeted<RetainedMapSnapshot<DocId, Value>>),
    Admitted(Budgeted<RetainedMapSnapshot<DocId, Budgeted<Value>>>),
}

pub(super) enum StoredValuesIter<'a> {
    Native(RetainedMapIter<'a, DocId, Value>),
    Admitted(RetainedMapIter<'a, DocId, Budgeted<Value>>),
}

impl<'a> Iterator for StoredValuesIter<'a> {
    type Item = (&'a DocId, &'a Value);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Native(values) => values.next(),
            Self::Admitted(values) => values.next().map(|(id, value)| (id, &**value)),
        }
    }
}

impl StoredValues {
    pub(super) fn new() -> Self {
        Self {
            root: Values::Native(RetainedMap::new(&MemoryBudget::new(usize::MAX))),
            cancellation: CancellationToken::new(),
        }
    }

    pub(super) fn get(&self, id: DocId) -> Option<&Value> {
        match &self.root {
            Values::Native(values) => values.get(&id),
            Values::Admitted(values) => values.get(&id).map(|value| &**value),
        }
    }

    pub(super) fn contains_key(&self, id: DocId) -> bool {
        self.get(id).is_some()
    }

    pub(super) fn insert(&mut self, id: DocId, value: Value) {
        let memory = match &self.root {
            Values::Native(values) => values.budget(),
            Values::Admitted(values) => values.budget(),
        };
        let bytes = value
            .retained_payload_bytes(memory, &self.cancellation)
            .expect("addressable native evaluated-key payload");
        match &mut self.root {
            Values::Native(values) => values.insert(id, value, bytes),
            Values::Admitted(values) => {
                let memory = values
                    .budget()
                    .reserve(bytes)
                    .expect("addressable native payload");
                values.insert(id, Budgeted::new(value, memory), bytes)
            }
        }
        .expect("addressable native evaluated-key root");
    }

    pub(super) fn remove(&mut self, id: DocId) {
        match &mut self.root {
            Values::Native(values) => values.remove(&id),
            Values::Admitted(values) => values.remove(&id),
        }
        .expect("addressable native evaluated-key path");
    }

    pub(super) fn clear(&mut self) {
        match &mut self.root {
            Values::Native(values) => values.clear(),
            Values::Admitted(values) => values.clear(),
        }
    }

    pub(super) fn capture(
        &self,
        memory: &MemoryBudget,
    ) -> Result<StoredValuesSnapshot, MemoryError> {
        Ok(match &self.root {
            Values::Native(values) => StoredValuesSnapshot::Native(values.capture(memory)?),
            Values::Admitted(values) => StoredValuesSnapshot::Admitted(values.capture(memory)?),
        })
    }

    pub(super) fn from_admitted(values: RetainedMap<DocId, Budgeted<Value>>) -> Self {
        Self {
            root: Values::Admitted(values.into_mutation_budget(&MemoryBudget::new(usize::MAX))),
            cancellation: CancellationToken::new(),
        }
    }
}

impl<'a> IntoIterator for &'a StoredValues {
    type Item = (&'a DocId, &'a Value);
    type IntoIter = StoredValuesIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        match &self.root {
            Values::Native(values) => StoredValuesIter::Native(values.iter()),
            Values::Admitted(values) => StoredValuesIter::Admitted(values.iter()),
        }
    }
}

impl StoredValuesSnapshot {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Native(values) => values.len(),
            Self::Admitted(values) => values.len(),
        }
    }

    pub(super) fn get(&self, id: DocId) -> Option<&Value> {
        match self {
            Self::Native(values) => values.get(&id),
            Self::Admitted(values) => values.get(&id).map(|value| &**value),
        }
    }

    pub(super) fn iter(&self) -> StoredValuesIter<'_> {
        match self {
            Self::Native(values) => StoredValuesIter::Native(values.iter()),
            Self::Admitted(values) => StoredValuesIter::Admitted(values.iter()),
        }
    }

    pub(super) fn first_from(&self, start: std::ops::Bound<&DocId>) -> Option<(&DocId, &Value)> {
        match self {
            Self::Native(values) => values.first_from(start),
            Self::Admitted(values) => values.first_from(start).map(|(id, value)| (id, &**value)),
        }
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
