//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind sequence owner validation and publication to current registry and transaction state.
use crate::Engine;
use uqa_execution::schema::sequences::owner_publication::{
    SequenceOwnerNames, SequenceOwnerPublicationContext,
};
use uqa_sql::SQLError;
use uqa_storage::{SequenceOwner, StorageBackendResult};
impl SequenceOwnerNames for Engine {
    fn resolve_sequence_name(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.try_resolve_sequence_name(name)
    }
}
impl Engine {
    pub(crate) fn sequence_owner_publication_context(&self) -> SequenceOwnerPublicationContext<'_> {
        SequenceOwnerPublicationContext {
            names: self,
            stored_names: self,
            sequences: self,
            definitions: self,
            publication: self,
        }
    }
    pub(crate) fn attach_sequence_owner_identity(
        &self,
        name: &str,
        owner: SequenceOwner,
    ) -> Result<(), SQLError> {
        self.sequence_owner_publication_context()
            .attach_sequence_owner_identity(name, owner)
    }
}
