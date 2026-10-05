//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attribute descriptors of one composite registry generation, indexed once per generation for per-row input and coercion.

use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::expr::composites::CompositeTypeDescriptor;

use super::CompositeRegistry;

#[derive(Default)]
pub struct CompositeDescriptorCache {
    entry: Mutex<Option<IndexedDescriptors>>,
}

struct IndexedDescriptors {
    registry: Arc<CompositeRegistry>,
    types: BTreeMap<u32, Arc<CompositeTypeDescriptor>>,
}

impl CompositeDescriptorCache {
    /// The live attributes of one composite type in the given registry generation.
    pub fn descriptor(
        &self,
        registry: &Arc<CompositeRegistry>,
        type_oid: u32,
    ) -> Option<Arc<CompositeTypeDescriptor>> {
        let mut entry = self.entry.lock();
        if !entry
            .as_ref()
            .is_some_and(|indexed| Arc::ptr_eq(&indexed.registry, registry))
        {
            *entry = Some(IndexedDescriptors {
                registry: Arc::clone(registry),
                types: registry
                    .values()
                    .map(|definition| (definition.oid, Arc::new(definition.descriptor())))
                    .collect(),
            });
        }
        entry
            .as_ref()
            .and_then(|indexed| indexed.types.get(&type_oid).cloned())
    }
}
