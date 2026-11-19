//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained schema restoration share identity uniqueness and error order.

use uqa_core::{
    catalog_schema::SchemaTupleIdentity,
    memory::{ProductionControl, ProductionMap},
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub(super) struct Identities<'a> {
    oids: ProductionMap<'a, i64, ()>,
    objects: ProductionMap<'a, [u8; 16], ()>,
}

impl<'a> Identities<'a> {
    pub(super) fn new(control: ProductionControl<'a>) -> Self {
        Self {
            oids: ProductionMap::new(control),
            objects: ProductionMap::new(control),
        }
    }

    pub(super) fn check(
        &mut self,
        name: &str,
        tuple: SchemaTupleIdentity,
    ) -> StorageBackendResult<()> {
        if self.oids.insert_copy(tuple.oid, ())?.is_some()
            || self.objects.insert_copy(tuple.object_id, ())?.is_some()
        {
            return Err(StorageBackendError::Other(format!(
                "duplicate schema catalog identity for `{name}`"
            )));
        }
        Ok(())
    }
}
