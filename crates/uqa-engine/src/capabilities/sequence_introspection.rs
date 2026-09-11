//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind sequence inspection to live registry guards and existing catalog resolution.

use crate::Engine;
use uqa_core::{RelationIdentity, Value};
use uqa_execution::catalog::{
    security::SequenceSecurity,
    sequence::SequenceState,
    sequence_introspection::{
        SequenceIntrospectionCatalog, SequenceIntrospectionContext, SequenceObjectIdsRead,
        SequenceStatesRead,
    },
};
use uqa_sql::{ast::RelationPersistence, SQLError};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn sequence_introspection_context(&self) -> SequenceIntrospectionContext<'_> {
        SequenceIntrospectionContext {
            catalog: self.catalog_execution(),
            sequences: self,
            owners: self,
            roles: self,
        }
    }
    pub(crate) fn pg_sequence_parameters_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        self.sequence_introspection_context()
            .pg_sequence_parameters_value(arguments)
    }
    pub(crate) fn pg_get_sequence_data_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        self.sequence_introspection_context()
            .pg_get_sequence_data_value(arguments)
    }
    pub(crate) fn pg_sequence_last_value_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        self.sequence_introspection_context()
            .pg_sequence_last_value_value(arguments)
    }
    pub(crate) fn pg_get_serial_sequence_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        self.sequence_introspection_context()
            .pg_get_serial_sequence_value(arguments)
    }
}
impl SequenceIntrospectionCatalog for Engine {
    fn refresh_sequences(&self) -> StorageBackendResult<()> {
        self.refresh_sequences_from_catalog()
    }
    fn object_ids(&self) -> SequenceObjectIdsRead<'_> {
        Box::new(self.durable.sequence_object_ids.read())
    }
    fn states(&self) -> SequenceStatesRead<'_> {
        Box::new(self.durable.sequences.read())
    }
    fn sequence_state(&self, relation: &RelationIdentity) -> Option<SequenceState> {
        self.durable.sequences.read().get(relation).copied()
    }
    fn sequence_security(&self, relation: &RelationIdentity) -> Option<SequenceSecurity> {
        self.durable.sequence_security.read().get(relation).cloned()
    }
    fn sequence_persistence(&self, relation: &RelationIdentity) -> Option<RelationPersistence> {
        self.durable
            .sequence_persistence
            .read()
            .get(relation)
            .copied()
    }
}
