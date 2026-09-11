//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend sequence registry guards and retain the original concrete publication order.
use crate::Engine;
use uqa_execution::catalog::sequence::restoration::{
    RestoredSequenceRegistry, SequencePersistenceRead, SequenceRestoreContext,
    SequenceRestoreRegistry,
};
impl Engine {
    pub(crate) fn sequence_restore_context(&self) -> SequenceRestoreContext<'_> {
        SequenceRestoreContext {
            sequences: self,
            security: self,
            registry: self,
        }
    }
}
impl SequenceRestoreRegistry for Engine {
    fn persistence(&self) -> SequencePersistenceRead<'_> {
        Box::new(self.durable.sequence_persistence.read())
    }
    fn install(&self, registry: RestoredSequenceRegistry) {
        *self.durable.sequences.write() = registry.sequences;
        *self.durable.sequence_object_ids.write() = registry.object_ids;
        *self.durable.sequence_persistence.write() = registry.persistence;
        *self.durable.sequence_security.write() = registry.security;
    }
}

#[cfg(test)]
mod tests;
