//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence rename and schema-move declaration rules.
use crate::{
    ast::{RelationPersistence, SequenceBound, SequenceLifecycle, SequenceOwnership},
    SQLError,
};
use uqa_core::RelationIdentity;
/// Namespace and ownership metadata needed to bind one sequence lifecycle target.
pub trait SequenceLifecycleCatalog {
    fn temporary_schema_name(&self) -> String;
    fn sequence_is_owned(&self, relation: &RelationIdentity) -> bool;
    fn schema_exists(&self, schema: &str) -> bool;
    fn current_user_name(&self) -> String;
    fn require_schema_create(&self, schema: &str, role: &str) -> Result<(), SQLError>;
    fn relation_kind_at(&self, name: &str) -> Result<Option<&'static str>, String>;
}
pub fn validate_sequence_lifecycle_shape(
    alter: &crate::ast::AlterSequence,
) -> Result<(), SQLError> {
    if alter.restart != crate::ast::SequenceRestart::Unchanged
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

pub fn sequence_lifecycle_target(
    catalog: &dyn SequenceLifecycleCatalog,
    source: &RelationIdentity,
    persistence: RelationPersistence,
    lifecycle: &SequenceLifecycle,
) -> Result<Option<RelationIdentity>, SQLError> {
    match lifecycle {
        SequenceLifecycle::Unchanged => Err(SQLError::Internal(
            "sequence lifecycle executor received no action".into(),
        )),
        SequenceLifecycle::RenameTo { name } => {
            let (schema, target_name) = RelationIdentity::parse_reference(name)
                .map_err(|error| SQLError::Internal(format!("invalid sequence name: {error}")))?;
            if schema.is_some() {
                return Err(SQLError::Internal(
                    "ALTER SEQUENCE RENAME TO produced a qualified target".into(),
                ));
            }
            let target = RelationIdentity::new(&source.schema, target_name);
            reject_sequence_lifecycle_collision(catalog, source, &target, true)?;
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
            let temporary_schema = catalog.temporary_schema_name();
            if schema == "pg_temp" {
                target_schema.clone_from(&temporary_schema);
            }
            if persistence == RelationPersistence::Temporary || target_schema == temporary_schema {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "cannot move objects into or out of temporary schemas".into(),
                });
            }
            if catalog.sequence_is_owned(source) {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "cannot move an owned sequence into another schema".into(),
                });
            }
            if !catalog.schema_exists(&target_schema) {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{target_schema}\" does not exist"),
                });
            }
            let current_user = catalog.current_user_name();
            catalog.require_schema_create(&target_schema, &current_user)?;
            let target = RelationIdentity::new(target_schema, &source.name);
            if target == *source {
                return Ok(None);
            }
            reject_sequence_lifecycle_collision(catalog, source, &target, false)?;
            Ok(Some(target))
        }
    }
}

fn reject_sequence_lifecycle_collision(
    catalog: &dyn SequenceLifecycleCatalog,
    source: &RelationIdentity,
    target: &RelationIdentity,
    rename: bool,
) -> Result<(), SQLError> {
    if target == source
        || catalog
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

use crate::catalog::resolution::RelationResolution;

pub fn alter_sequence_target_name(
    resolution: crate::catalog::resolution::RelationResolution,
    alter: &crate::ast::AlterSequence,
) -> Result<Option<String>, SQLError> {
    match resolution {
        RelationResolution::Found(name, "sequence") => Ok(Some(name)),
        RelationResolution::Found(_name, _kind) => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{}\" is not a sequence", alter.name),
        }),
        RelationResolution::MissingRelation | RelationResolution::MissingSchema(_)
            if alter.if_exists =>
        {
            Ok(None)
        }
        RelationResolution::MissingSchema(schema) => Err(SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{schema}\" does not exist"),
        }),
        RelationResolution::MissingRelation => Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: format!("relation \"{}\" does not exist", alter.name),
        }),
    }
}
