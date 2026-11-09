//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact field layouts shared across immutable document snapshots.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    ops::{Deref, DerefMut, Index},
    sync::Arc,
};

use super::{Document, Value};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryReservation, OwnedMap};

#[derive(Debug, Clone)]
enum LayoutFields {
    Ordinary(Arc<[String]>),
    Retained(Arc<Budgeted<Vec<String>>>),
}

impl Deref for LayoutFields {
    type Target = [String];
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Ordinary(fields) => fields,
            Self::Retained(fields) => fields,
        }
    }
}

#[derive(Debug, Default)]
struct LayoutCandidates {
    ids: Vec<usize>,
    image_memory: Option<MemoryReservation>,
}

impl Clone for LayoutCandidates {
    fn clone(&self) -> Self {
        Self {
            ids: self.ids.clone(),
            image_memory: None,
        }
    }
}

impl Deref for LayoutCandidates {
    type Target = Vec<usize>;
    fn deref(&self) -> &Self::Target {
        &self.ids
    }
}

impl DerefMut for LayoutCandidates {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ids
    }
}

#[derive(Debug, Default)]
pub(super) struct DocumentLayouts {
    fields: Vec<LayoutFields>,
    identities: OwnedMap<u64, LayoutCandidates>,
    hasher: RandomState,
    field_bytes: u128,
    candidate_bytes: u128,
    image_memory: Option<MemoryReservation>,
}

impl DocumentLayouts {
    pub(super) fn retained_bytes(&self) -> u128 {
        self.field_bytes
            + self.candidate_bytes
            + self.fields.capacity() as u128 * size_of::<LayoutFields>() as u128
            + self.identities.allocated_bytes() as u128
    }

    pub(super) fn store(&mut self, document: Document) -> (usize, Vec<Value>) {
        let hash = self.hash(&document);
        if let Some(id) = self.identities.get(&hash).and_then(|candidates| {
            candidates.iter().copied().find(|id| {
                #[cfg(test)]
                COMPARISONS.with(|count| count.set(count.get() + 1));
                let fields = &self.fields[*id];
                fields.len() == document.len()
                    && fields
                        .iter()
                        .map(String::as_str)
                        .eq(document.keys().map(String::as_str))
            })
        }) {
            return (id, document.into_values().collect());
        }
        let (fields, values): (Vec<_>, Vec<_>) = document.into_iter().unzip();
        let id = self.fields.len();
        self.field_bytes += fields.len() as u128 * size_of::<String>() as u128
            + fields
                .iter()
                .map(|field| field.capacity() as u128)
                .sum::<u128>();
        self.fields.push(LayoutFields::Ordinary(fields.into()));
        if !self.identities.contains_key(&hash) {
            self.identities.insert(hash, LayoutCandidates::default());
        }
        let candidates = self
            .identities
            .get_mut(&hash)
            .expect("published layout hash");
        let previous = candidates.capacity();
        candidates.push(id);
        self.candidate_bytes +=
            (candidates.capacity() - previous) as u128 * size_of::<usize>() as u128;
        (id, values)
    }

    fn hash(&self, document: &Document) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        document.len().hash(&mut hasher);
        for name in document.keys() {
            name.hash(&mut hasher);
        }
        hasher.finish()
    }

    pub(super) fn for_images(control: &StorageReadControl) -> Self {
        Self {
            image_memory: Some(control.memory().empty_reservation()),
            ..Self::default()
        }
    }

    /// The unpublished builder owns this layout set; any failure consumes that builder and its partial rows.
    pub(super) fn store_image_fields(
        &mut self,
        names: Budgeted<Vec<String>>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<usize> {
        control.check()?;
        let mut hasher = self.hasher.build_hasher();
        names.len().hash(&mut hasher);
        for name in names.iter() {
            name.hash(&mut hasher);
        }
        let hash = hasher.finish();
        if let Some(id) = self.identities.get(&hash).and_then(|candidates| {
            candidates
                .iter()
                .copied()
                .find(|id| &*self.fields[*id] == names.as_slice())
        }) {
            return Ok(id);
        }
        let names = names.into_shared()?;
        let name_bytes = names.reserved_bytes() as u128;
        let id = self.fields.len();
        let mut fields = BudgetedVec::from_parts(
            std::mem::take(&mut self.fields),
            self.image_memory.take().expect("unpublished image layouts"),
        );
        fields.push(LayoutFields::Retained(names))?;
        let (fields, memory) = fields.into_parts();
        self.fields = fields;
        self.image_memory = Some(memory);
        self.field_bytes += name_bytes;
        if let Some(candidates) = self.identities.get_mut(&hash) {
            let old_capacity = candidates.ids.capacity();
            let mut ids = BudgetedVec::from_parts(
                std::mem::take(&mut candidates.ids),
                candidates
                    .image_memory
                    .take()
                    .expect("unpublished image candidates"),
            );
            ids.push(id)?;
            let (ids, memory) = ids.into_parts();
            self.candidate_bytes +=
                (ids.capacity() - old_capacity) as u128 * size_of::<usize>() as u128;
            candidates.ids = ids;
            candidates.image_memory = Some(memory);
        } else {
            let mut ids = BudgetedVec::new(control.memory());
            ids.push(id)?;
            self.candidate_bytes += ids.capacity() as u128 * size_of::<usize>() as u128;
            let (ids, memory) = ids.into_parts();
            self.identities.insert_budgeted(
                hash,
                LayoutCandidates {
                    ids,
                    image_memory: Some(memory),
                },
                control.memory(),
            )?;
        }
        control.check()?;
        Ok(id)
    }
}

impl Clone for DocumentLayouts {
    fn clone(&self) -> Self {
        let identities = self.identities.clone();
        let candidate_bytes = identities
            .values()
            .map(|candidates| candidates.capacity() as u128 * size_of::<usize>() as u128)
            .sum();
        Self {
            fields: self.fields.clone(),
            identities,
            hasher: self.hasher.clone(),
            field_bytes: self.field_bytes,
            candidate_bytes,
            image_memory: None,
        }
    }
}

impl Index<usize> for DocumentLayouts {
    type Output = [String];

    fn index(&self, id: usize) -> &Self::Output {
        &self.fields[id]
    }
}

#[cfg(test)]
thread_local! {
    static COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(index: usize, value: i64) -> Document {
        Document::from([(format!("field_{index}"), Value::Int(value))])
    }

    #[test]
    fn layout_discovery_checks_only_the_indexed_candidates() {
        let mut layouts = DocumentLayouts::default();
        for index in 0..2048 {
            assert_eq!(layouts.store(document(index, 0)).0, index);
        }
        let before = COMPARISONS.with(std::cell::Cell::get);
        for index in (0..2048).rev() {
            let (id, values) = layouts.store(document(index, index as i64));
            assert_eq!(id, index);
            assert_eq!(values, [Value::Int(index as i64)]);
        }
        let comparisons = COMPARISONS.with(std::cell::Cell::get) - before;
        assert_eq!(comparisons, 2048);
        assert_eq!(layouts.fields.len(), 2048);
    }

    #[test]
    fn layout_hash_candidates_still_require_exact_field_identity() {
        let mut layouts = DocumentLayouts::default();
        assert_eq!(layouts.store(document(0, 10)).0, 0);
        assert_eq!(layouts.store(document(1, 20)).0, 1);
        let hash = layouts.hash(&document(1, 30));
        layouts.identities.get_mut(&hash).unwrap().insert(0, 0);
        assert_eq!(layouts.store(document(1, 30)), (1, vec![Value::Int(30)]));
        assert_eq!(&layouts[0], ["field_0"]);
        assert_eq!(&layouts[1], ["field_1"]);
    }

    #[test]
    fn cloned_layout_indexes_share_names_and_keep_independent_additions() {
        let mut layouts = DocumentLayouts::default();
        layouts.store(document(0, 10));
        let original = layouts.fields[0].clone();
        let mut cloned = layouts.clone();
        assert_eq!(cloned.store(document(0, 20)).0, 0);
        assert_eq!(cloned.store(document(1, 30)).0, 1);
        assert!(std::ptr::eq(original.as_ptr(), cloned.fields[0].as_ptr()));
        assert_eq!(layouts.fields.len(), 1);
        assert_eq!(cloned.fields.len(), 2);
        assert_eq!(layouts.store(Document::new()), (1, Vec::new()));
        assert_eq!(cloned.store(Document::new()), (2, Vec::new()));
    }
}
