//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact field layouts shared across immutable document snapshots.

use std::{
    collections::HashMap,
    hash::{BuildHasher, Hash, Hasher},
    ops::Index,
    sync::Arc,
};

use super::{Document, Value};

#[derive(Debug, Default, Clone)]
pub(super) struct DocumentLayouts {
    fields: Vec<Arc<[String]>>,
    identities: HashMap<u64, Vec<usize>>,
}

impl DocumentLayouts {
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
        self.fields.push(fields.into());
        self.identities.entry(hash).or_default().push(id);
        (id, values)
    }

    fn hash(&self, document: &Document) -> u64 {
        let mut hasher = self.identities.hasher().build_hasher();
        document.len().hash(&mut hasher);
        for name in document.keys() {
            name.hash(&mut hasher);
        }
        hasher.finish()
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
        let original = Arc::clone(&layouts.fields[0]);
        let mut cloned = layouts.clone();
        assert_eq!(cloned.store(document(0, 20)).0, 0);
        assert_eq!(cloned.store(document(1, 30)).0, 1);
        assert!(Arc::ptr_eq(&original, &cloned.fields[0]));
        assert_eq!(layouts.fields.len(), 1);
        assert_eq!(cloned.fields.len(), 2);
        assert_eq!(layouts.store(Document::new()), (1, Vec::new()));
        assert_eq!(cloned.store(Document::new()), (2, Vec::new()));
    }
}
