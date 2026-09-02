//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence name and namespace lifecycle with stable catalog identity.

use super::{Engine, RelationIdentity, SQLError};
use uqa_sql::ast::{RelationPersistence, SequenceBound, SequenceLifecycle, SequenceOwnership};

impl Engine {
    pub(crate) fn alter_sequence_lifecycle_inner(
        &self,
        source_name: &str,
        source: &RelationIdentity,
        persistence: RelationPersistence,
        alter: &uqa_sql::ast::AlterSequence,
    ) -> Result<(), SQLError> {
        Self::validate_sequence_lifecycle_shape(alter)?;
        let Some(target) = self.sequence_lifecycle_target(source, persistence, &alter.lifecycle)?
        else {
            return Ok(());
        };
        let target_name = target.qualified_name();
        self.rewrite_sequence_schema_dependencies(source, &target_name)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "rewrite table dependencies for sequence `{source_name}`: {error}"
                ))
            })?;
        self.rewrite_view_sequence_references(source, &target_name)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "rewrite view dependencies for sequence `{source_name}`: {error}"
                ))
            })?;
        if persistence == RelationPersistence::Temporary {
            self.move_sequence_state(source, &target)?;
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            if !catalog
                .rename_sequence_row(source_name, &target_name)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "persist sequence rename `{source_name}` to `{target_name}`: {error}"
                    ))
                })?
            {
                return Err(SQLError::Internal(format!(
                    "sequence `{source_name}` disappeared during rename"
                )));
            }
            self.refresh_sequences_from_catalog().map_err(|error| {
                SQLError::Internal(format!(
                    "refresh sequence `{target_name}` after rename: {error}"
                ))
            })?;
        } else {
            self.move_sequence_state(source, &target)?;
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn validate_sequence_lifecycle_shape(
        alter: &uqa_sql::ast::AlterSequence,
    ) -> Result<(), SQLError> {
        if alter.restart != uqa_sql::ast::SequenceRestart::Unchanged
            || alter.increment.is_some()
            || alter.start.is_some()
            || alter.data_type.is_some()
            || alter.min_value != SequenceBound::Unchanged
            || alter.max_value != SequenceBound::Unchanged
            || alter.cycle.is_some()
            || alter.cache_size.is_some()
            || alter.ownership != SequenceOwnership::Unchanged
            || alter.persistence.is_some()
            || alter.role_owner.is_some()
        {
            return Err(SQLError::Internal(
                "ALTER SEQUENCE name lifecycle cannot contain definition changes".into(),
            ));
        }
        Ok(())
    }

    fn sequence_lifecycle_target(
        &self,
        source: &RelationIdentity,
        persistence: RelationPersistence,
        lifecycle: &SequenceLifecycle,
    ) -> Result<Option<RelationIdentity>, SQLError> {
        match lifecycle {
            SequenceLifecycle::Unchanged => Err(SQLError::Internal(
                "sequence lifecycle executor received no action".into(),
            )),
            SequenceLifecycle::RenameTo { name } => {
                let (schema, target_name) =
                    RelationIdentity::parse_reference(name).map_err(|error| {
                        SQLError::Internal(format!("invalid sequence name: {error}"))
                    })?;
                if schema.is_some() {
                    return Err(SQLError::Internal(
                        "ALTER SEQUENCE RENAME TO produced a qualified target".into(),
                    ));
                }
                let target = RelationIdentity::new(&source.schema, target_name);
                self.reject_sequence_lifecycle_collision(source, &target, true)?;
                Ok(Some(target))
            }
            SequenceLifecycle::SetSchema { schema } => {
                let (qualifier, mut target_schema) = RelationIdentity::parse_reference(schema)
                    .map_err(|error| SQLError::Internal(format!("invalid schema name: {error}")))?;
                if qualifier.is_some() {
                    return Err(SQLError::Internal(
                        "ALTER SEQUENCE SET SCHEMA produced a qualified schema".into(),
                    ));
                }
                let temporary_schema = self.temporary_schema_name();
                if schema == "pg_temp" {
                    target_schema.clone_from(&temporary_schema);
                }
                if persistence == RelationPersistence::Temporary
                    || target_schema == temporary_schema
                {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "cannot move objects into or out of temporary schemas".into(),
                    });
                }
                if self
                    .durable
                    .sequences
                    .read()
                    .get(source)
                    .is_some_and(|state| state.owner.is_some())
                {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "cannot move an owned sequence into another schema".into(),
                    });
                }
                if !self.durable.schemas.read().contains_key(&target_schema) {
                    return Err(SQLError::Routine {
                        sqlstate: "3F000".into(),
                        message: format!("schema \"{target_schema}\" does not exist"),
                    });
                }
                let current_user = self.current_user_name();
                self.ensure_schema_privilege(
                    &target_schema,
                    &current_user,
                    crate::engine_schema_security::SchemaAclPrivilege::Create,
                )?;
                let target = RelationIdentity::new(target_schema, &source.name);
                if target == *source {
                    return Ok(None);
                }
                self.reject_sequence_lifecycle_collision(source, &target, false)?;
                Ok(Some(target))
            }
        }
    }

    fn reject_sequence_lifecycle_collision(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
        rename: bool,
    ) -> Result<(), SQLError> {
        if target == source
            || self
                .relation_kind_at(&target.qualified_name())
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "check sequence lifecycle target `{}`: {error}",
                        target.qualified_name()
                    ))
                })?
                .is_some()
        {
            return Err(SQLError::Routine {
                sqlstate: "42P07".into(),
                message: if rename {
                    format!("relation \"{}\" already exists", target.name)
                } else {
                    format!(
                        "relation \"{}\" already exists in schema \"{}\"",
                        target.name, target.schema
                    )
                },
            });
        }
        Ok(())
    }

    fn move_sequence_state(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
    ) -> Result<(), SQLError> {
        let mut sequences = self.durable.sequences.write();
        let mut object_ids = self.durable.sequence_object_ids.write();
        let mut persistence = self.durable.sequence_persistence.write();
        let mut security = self.durable.sequence_security.write();
        if !sequences.contains_key(source)
            || !object_ids.contains_key(source)
            || !persistence.contains_key(source)
            || !security.contains_key(source)
        {
            return Err(SQLError::Internal(format!(
                "sequence registry entry `{}` disappeared during rename",
                source.qualified_name()
            )));
        }
        if sequences.contains_key(target)
            || object_ids.contains_key(target)
            || persistence.contains_key(target)
            || security.contains_key(target)
        {
            return Err(SQLError::Internal(format!(
                "sequence registry target `{}` appeared during rename",
                target.qualified_name()
            )));
        }
        let state = sequences
            .remove(source)
            .expect("preflighted sequence state must exist");
        let object_id = object_ids
            .remove(source)
            .expect("preflighted sequence object identity must exist");
        let stored_persistence = persistence
            .remove(source)
            .expect("preflighted sequence persistence must exist");
        let stored_security = security
            .remove(source)
            .expect("preflighted sequence security must exist");
        sequences.insert(target.clone(), state);
        object_ids.insert(target.clone(), object_id);
        persistence.insert(target.clone(), stored_persistence);
        security.insert(target.clone(), stored_security);
        Ok(())
    }
}
