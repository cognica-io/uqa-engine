//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only range layouts are shared by both type OIDs for one immutable catalog generation.

use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{expr::RangeValueDescriptor, SQLError};

use super::super::{cache::CatalogDerivation, CatalogReadView};

#[derive(Default)]
pub(crate) struct RangeDescriptors {
    values: CatalogDerivation<BTreeMap<u32, Arc<RangeValueDescriptor>>>,
}

impl CatalogReadView {
    /// Borrow pinned layout support without resolving mutable names or invoking endpoint input. An empty range registry adds no descriptor-cache allocation.
    pub fn range_value_descriptor(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<RangeValueDescriptor>>, SQLError> {
        let Some(cache) = &self.range_descriptors else {
            return Ok(None);
        };
        let descriptors = cache.values.get_or_try_init(|| {
            let mut descriptors = BTreeMap::new();
            for range in self.ranges() {
                let descriptor = Arc::new(range.descriptor());
                for oid in [range.oid, range.multirange.oid] {
                    if descriptors.insert(oid, Arc::clone(&descriptor)).is_some() {
                        return Err(SQLError::Internal(format!(
                            "duplicate range type OID {oid}"
                        )));
                    }
                }
            }
            Ok(descriptors)
        })?;
        Ok(descriptors.get(&oid).cloned())
    }
}
