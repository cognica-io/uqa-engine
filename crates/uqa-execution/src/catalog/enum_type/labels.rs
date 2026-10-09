//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum label lists served to expression evaluation, indexed once per registry generation.

use super::EnumRegistry;
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::EnumValue;
use uqa_sql::expr::enums::{EnumTypeLabel, EnumTypeLabels};

/// Label lists by type OID for the most recently read registry generation. Every publication installs a new registry `Arc`, so a pointer comparison detects a stale index without an invalidation hook; the cache holds its generation, so the pointer cannot be reused while it is cached.
#[derive(Default)]
pub struct EnumLabelCache {
    entry: Mutex<Option<IndexedLabels>>,
}

struct IndexedLabels {
    registry: Arc<EnumRegistry>,
    types: BTreeMap<u32, Arc<EnumTypeLabels>>,
    labels: BTreeMap<u32, (u32, usize)>,
}

impl IndexedLabels {
    fn new(registry: &Arc<EnumRegistry>) -> Self {
        let types: BTreeMap<_, _> = registry
            .values()
            .map(|definition| {
                let labels = definition
                    .labels
                    .iter()
                    .map(|label| EnumTypeLabel {
                        oid: label.oid,
                        key: label.key.clone(),
                        label: label.label.clone(),
                    })
                    .collect();
                (
                    definition.oid,
                    Arc::new(EnumTypeLabels {
                        type_oid: definition.oid,
                        labels,
                    }),
                )
            })
            .collect();
        let labels = types
            .iter()
            .flat_map(|(type_oid, labels)| {
                labels
                    .labels
                    .iter()
                    .enumerate()
                    .map(|(index, label)| (label.oid, (*type_oid, index)))
            })
            .collect();
        Self {
            registry: Arc::clone(registry),
            types,
            labels,
        }
    }
}

impl EnumLabelCache {
    /// The labels of one enum type in the given registry generation, in ascending key order.
    pub fn labels(
        &self,
        registry: &Arc<EnumRegistry>,
        type_oid: u32,
    ) -> Option<Arc<EnumTypeLabels>> {
        self.with_index(registry, |indexed| indexed.types.get(&type_oid).cloned())
    }

    /// Look up an admitted physical label identity in the same pinned generation as ordinary enum output.
    pub fn value(&self, registry: &Arc<EnumRegistry>, label_oid: u32) -> Option<EnumValue> {
        self.with_index(registry, |indexed| {
            let (type_oid, index) = indexed.labels.get(&label_oid)?;
            let label = &indexed.types.get(type_oid)?.labels[*index];
            Some(EnumValue::new(*type_oid, label.key.clone()).with_label_oid(Some(label_oid)))
        })
    }

    fn with_index<T>(
        &self,
        registry: &Arc<EnumRegistry>,
        read: impl FnOnce(&IndexedLabels) -> T,
    ) -> T {
        let mut entry = self.entry.lock();
        if !entry
            .as_ref()
            .is_some_and(|indexed| Arc::ptr_eq(&indexed.registry, registry))
        {
            *entry = Some(IndexedLabels::new(registry));
        }
        read(entry.as_ref().expect("indexed registry generation"))
    }
}
