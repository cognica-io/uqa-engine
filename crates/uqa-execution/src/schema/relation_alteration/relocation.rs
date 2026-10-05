//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move a locked relation and then its indexes and owned sequences in `PostgreSQL` catalog order.

use crate::row_locks::binding::{bind_relation, RelationBinding, RelationDefinitionSession};
use crate::row_locks::RelationLockMode;
use crate::schema::{
    indexes::registry::IndexRegistryContext, namespaces::relations::RelationCreationContext,
    sequences::lifecycle::SequenceLifecycleContext,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::RelationPersistence,
    schema::relation_alteration::{relocation, RelationAlterNames},
    SQLError,
};

pub struct RelationSchemaContext<'a> {
    pub creation: RelationCreationContext<'a>,
    pub names: &'a dyn RelationAlterNames,
    pub locks: &'a dyn RelationDefinitionSession,
    pub indexes: IndexRegistryContext<'a>,
    pub sequences: SequenceLifecycleContext<'a>,
}

pub struct PreparedRelationSchemaMove {
    indexes: crate::schema::indexes::relocation::PreparedIndexRelocations,
    sequences: Vec<PreparedSequenceRelocation>,
}

struct PreparedSequenceRelocation {
    source: RelationIdentity,
    target: RelationIdentity,
    persistence: RelationPersistence,
}

impl PreparedRelationSchemaMove {
    /// Publish only retained definitions after the owner and its types have moved, without another lock wait or catalog refresh.
    pub fn publish(self, context: &RelationSchemaContext<'_>) -> Result<(), SQLError> {
        self.indexes.publish(context.indexes.publication)?;
        for sequence in self.sequences {
            crate::schema::sequences::lifecycle::publish_relocation(
                &context.sequences,
                &sequence.source,
                &sequence.target,
                sequence.persistence,
            )?;
        }
        Ok(())
    }
}

impl RelationSchemaContext<'_> {
    pub fn target(
        &self,
        source: &RelationIdentity,
        schema: &str,
        persistence: RelationPersistence,
    ) -> Result<Option<RelationIdentity>, SQLError> {
        let declared = relocation::declared_target(source, schema)?;
        let target = self.creation.relocation_target(&declared)?;
        Ok(relocation::validate_target(
            self.names,
            source,
            &target,
            persistence,
            &self.creation.state.temporary_schema_name(),
        )?
        .then_some(target))
    }

    /// Reserve names in catalog order while every relation still has its original namespace. The source relation lock is already retained by the caller.
    pub fn prepare(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
        object_id: [u8; 16],
    ) -> Result<PreparedRelationSchemaMove, SQLError> {
        self.reserve_row_types(source, target)?;
        let indexes = crate::schema::indexes::relocation::prepare_table_indexes(
            &self.indexes,
            &self.creation,
            self.names,
            source,
            target,
        )?;
        let catalog = self.indexes.identities.catalog.current_catalog_snapshot();
        let definitions = &catalog.snapshot().definitions;
        let mut owned = definitions
            .sequences
            .iter()
            .filter_map(|(relation, state)| {
                state
                    .owner
                    .filter(|owner| owner.table_object_id == object_id)
                    .and_then(|_| definitions.sequence_object_ids.get(relation).copied())
            })
            .collect::<Vec<_>>();
        owned.sort_by_key(|identity| catalog.sequence_catalog_oid(identity));
        let mut sequences = Vec::new();
        for sequence_id in owned {
            if let Some(sequence) =
                self.prepare_owned_sequence(sequence_id, object_id, &target.schema)?
            {
                sequences.push(sequence);
            }
        }
        self.locks.prepare_definition_write()?;
        Ok(PreparedRelationSchemaMove { indexes, sequences })
    }

    fn reserve_row_types(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
    ) -> Result<(), SQLError> {
        let catalog = self.indexes.identities.catalog.current_catalog_snapshot();
        let snapshot = catalog.snapshot();
        let (array_name, array_oid) = if let Some(table) = snapshot.tables.get(source) {
            (
                table.row_type_array_name.as_deref(),
                table.catalog_oids.array_type,
            )
        } else if let Some(view) = snapshot.definitions.views.get(source) {
            (
                view.row_type_array_name.as_deref(),
                view.relation_oids().array_type,
            )
        } else if let Some(table) = snapshot.definitions.foreign_tables.get(source) {
            (
                table.row_type_array_name.as_deref(),
                table.relation_oids().array_type,
            )
        } else {
            return Err(SQLError::Internal(
                "moved relation disappeared before name reservation".into(),
            ));
        };
        crate::schema::types::relation_arrays::rename(
            &self.creation,
            source,
            target,
            array_name,
            array_oid,
        )?;
        Ok(())
    }

    fn prepare_owned_sequence(
        &self,
        sequence_id: [u8; 16],
        table_id: [u8; 16],
        schema: &str,
    ) -> Result<Option<PreparedSequenceRelocation>, SQLError> {
        let Some(binding) = bind_relation(
            self.locks,
            RelationLockMode::AccessExclusive,
            false,
            || {
                let catalog = self.indexes.identities.catalog.current_catalog_snapshot();
                let definitions = &catalog.snapshot().definitions;
                let Some((relation, _)) = definitions
                    .sequence_object_ids
                    .iter()
                    .find(|(_, id)| **id == sequence_id)
                else {
                    return Ok(None);
                };
                let Some(state) = definitions.sequences.get(relation) else {
                    return Ok(None);
                };
                if state
                    .owner
                    .is_none_or(|owner| owner.table_object_id != table_id)
                {
                    return Ok(None);
                }
                Ok(Some(RelationBinding {
                    name: relation.qualified_name(),
                    object_id: Some(sequence_id),
                    value: (
                        relation.clone(),
                        definitions
                            .sequence_persistence
                            .get(relation)
                            .copied()
                            .unwrap_or_default(),
                    ),
                }))
            },
            |_| Ok(()),
        )?
        else {
            return Ok(None);
        };
        let (source, persistence) = binding.value;
        let target = RelationIdentity::new(schema, &source.name);
        if !relocation::validate_target(
            self.names,
            &source,
            &target,
            persistence,
            &self.creation.state.temporary_schema_name(),
        )? {
            return Ok(None);
        }
        self.creation.reserve_name(&target.qualified_name())?;
        Ok(Some(PreparedSequenceRelocation {
            source,
            target,
            persistence,
        }))
    }
}
