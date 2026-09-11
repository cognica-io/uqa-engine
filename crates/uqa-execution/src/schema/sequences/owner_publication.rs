//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate loaded implicit owners and publish stable sequence-owner identities.
use super::alteration::{SequenceDefinitionCatalog, SequenceDefinitionPublication};
use crate::catalog::sequence_introspection::SequenceIntrospectionCatalog;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::ColumnDef, schema::sequences::implicit_ownership::StoredSequenceNames, SQLError,
};
use uqa_storage::{SequenceOwner, StorageBackendError, StorageBackendResult};
pub trait SequenceOwnerNames {
    fn resolve_sequence_name(&self, name: &str) -> StorageBackendResult<Option<String>>;
}
pub struct SequenceOwnerPublicationContext<'a> {
    pub names: &'a dyn SequenceOwnerNames,
    pub stored_names: &'a dyn StoredSequenceNames,
    pub sequences: &'a dyn SequenceIntrospectionCatalog,
    pub definitions: &'a dyn SequenceDefinitionCatalog,
    pub publication: &'a dyn SequenceDefinitionPublication,
    pub new_generation: fn() -> StorageBackendResult<[u8; 16]>,
}
impl SequenceOwnerPublicationContext<'_> {
    pub fn validate_implicit_sequence_owners_for_columns(
        &self,
        table_name: &str,
        table_object_id: [u8; 16],
        columns: &[ColumnDef],
    ) -> StorageBackendResult<()> {
        for (sequence, expected) in super::ownership::implicit_owner_bindings(
            self.stored_names,
            table_name,
            table_object_id,
            columns,
        )? {
            let relation = RelationIdentity::from_legacy_name(&sequence)
                .map_err(StorageBackendError::Other)?;
            let actual = self
                .sequences
                .states()
                .get(&relation)
                .and_then(|state| state.owner);
            if actual != Some(expected) {
                return Err(StorageBackendError::Other(format!(
                    "implicit sequence `{sequence}` for `{table_name}` has stale owner metadata that requires an initial-open migration"
                )));
            }
        }
        Ok(())
    }
    pub fn attach_sequence_owner_identity(
        &self,
        name: &str,
        owner: SequenceOwner,
    ) -> Result<(), SQLError> {
        let canonical = self
            .names
            .resolve_sequence_name(name)
            .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?
            .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
        let relation = RelationIdentity::from_legacy_name(&canonical)
            .map_err(StorageBackendError::Other)
            .map_err(|error| {
                SQLError::Internal(format!("resolve sequence `{canonical}`: {error}"))
            })?;
        let persistence = self
            .sequences
            .sequence_persistence(&relation)
            .unwrap_or_default();
        let object_id = self.definitions.object_id(&relation).ok_or_else(|| {
            SQLError::Internal(format!("sequence `{canonical}` has no object identity"))
        })?;
        let mut state = self
            .definitions
            .state(&relation)
            .ok_or_else(|| SQLError::Internal(format!("sequence `{canonical}` disappeared")))?;
        if state.owner == Some(owner) {
            return Ok(());
        }
        if state.owner.is_some() {
            return Err(SQLError::Internal(format!(
                "implicit sequence `{canonical}` already has another owner"
            )));
        }
        state.owner = Some(owner);
        state.definition_generation = (self.new_generation)().map_err(|error| {
            SQLError::Internal(format!(
                "allocate sequence `{canonical}` definition generation: {error}"
            ))
        })?;
        self.publication.replace_sequence(
            &canonical,
            &relation,
            object_id,
            persistence,
            state,
            true,
        )
    }
}
