//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind sequence inspection to live registry guards and existing catalog resolution.

use crate::Engine;
use uqa_execution::catalog::{
    sequence::registry::SequenceCatalogRead,
    sequence_introspection::{SequenceIntrospectionCatalog, SequenceIntrospectionContext},
};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn sequence_introspection_context(&self) -> SequenceIntrospectionContext<'_> {
        SequenceIntrospectionContext {
            catalog: self.catalog_execution(),
            snapshots: self,
            owners: self,
        }
    }
}
impl SequenceIntrospectionCatalog for Engine {
    fn refresh_sequences(&self) -> StorageBackendResult<()> {
        self.refresh_sequences_from_catalog()
    }
    fn registry(&self) -> SequenceCatalogRead<'_> {
        Box::new(self.durable.sequences.read())
    }
}
