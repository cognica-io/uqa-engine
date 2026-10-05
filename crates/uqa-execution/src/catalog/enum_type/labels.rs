//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum label lists served to expression evaluation, indexed once per registry generation.

use super::EnumRegistry;
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::expr::enums::{EnumTypeLabel, EnumTypeLabels};

/// Label lists by type OID for the most recently read registry generation. Every publication installs a new registry `Arc`, so a pointer comparison detects a stale index without an invalidation hook; the cache holds its generation, so the pointer cannot be reused while it is cached.
#[derive(Default)]
pub struct EnumLabelCache {
    entry: Mutex<Option<IndexedLabels>>,
}

struct IndexedLabels {
    registry: Arc<EnumRegistry>,
    types: BTreeMap<u32, Arc<EnumTypeLabels>>,
}

impl IndexedLabels {
    fn new(registry: &Arc<EnumRegistry>) -> Self {
        let types = registry
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
        Self {
            registry: Arc::clone(registry),
            types,
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
        let mut entry = self.entry.lock();
        if !entry
            .as_ref()
            .is_some_and(|indexed| Arc::ptr_eq(&indexed.registry, registry))
        {
            *entry = Some(IndexedLabels::new(registry));
        }
        entry
            .as_ref()
            .and_then(|indexed| indexed.types.get(&type_oid).cloned())
    }
}
