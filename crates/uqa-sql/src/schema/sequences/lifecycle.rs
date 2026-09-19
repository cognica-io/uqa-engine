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

/// Bind the declared destination before acquiring its namespace lock. Collision and namespace-kind checks require the refreshed catalog after that acquisition.
pub fn sequence_lifecycle_target(
    catalog: &dyn SequenceLifecycleCatalog,
    source: &RelationIdentity,
    lifecycle: &SequenceLifecycle,
) -> Result<RelationIdentity, SQLError> {
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
            Ok(RelationIdentity::new(&source.schema, target_name))
        }
        SequenceLifecycle::SetSchema { schema } => {
            let (qualifier, target_schema) = RelationIdentity::parse_reference(schema)
                .map_err(|error| SQLError::Internal(format!("invalid schema name: {error}")))?;
            if qualifier.is_some() {
                return Err(SQLError::Internal(
                    "ALTER SEQUENCE SET SCHEMA produced a qualified schema".into(),
                ));
            }
            if catalog.sequence_is_owned(source) {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "cannot move an owned sequence into another schema".into(),
                });
            }
            Ok(RelationIdentity::new(target_schema, &source.name))
        }
    }
}

/// Validate the locked destination and report whether its name needs publication. Even an unchanged schema must first retain its namespace dependency and pass namespace-kind validation.
pub fn validate_sequence_lifecycle_target(
    catalog: &dyn SequenceLifecycleCatalog,
    source: &RelationIdentity,
    target: &RelationIdentity,
    persistence: RelationPersistence,
    lifecycle: &SequenceLifecycle,
) -> Result<bool, SQLError> {
    let rename = matches!(lifecycle, SequenceLifecycle::RenameTo { .. });
    if !rename {
        if persistence == RelationPersistence::Temporary
            || target.schema == catalog.temporary_schema_name()
        {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot move objects into or out of temporary schemas".into(),
            });
        }
        if source.schema == "pg_toast" || target.schema == "pg_toast" {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot move objects into or out of TOAST schema".into(),
            });
        }
        if target == source {
            return Ok(false);
        }
    }
    reject_sequence_lifecycle_collision(catalog, source, target, rename)?;
    Ok(true)
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

/// Resolve absence before execution checks the actual relation's owner, namespace and requested kind.
pub fn sequence_alter_relation(
    resolution: crate::catalog::resolution::RelationResolution,
    alter: &crate::ast::AlterSequence,
) -> Result<Option<(String, &'static str)>, SQLError> {
    match resolution {
        RelationResolution::Found(name, kind) => Ok(Some((name, kind))),
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

pub fn validate_sequence_alter_kind(
    alter: &crate::ast::AlterSequence,
    kind: &str,
    local_name: &str,
) -> Result<(), SQLError> {
    if kind == "sequence" {
        return Ok(());
    }
    let definition = alter.lifecycle == SequenceLifecycle::Unchanged
        && alter.role_owner.is_none()
        && alter.persistence.is_none();
    Err(SQLError::Routine {
        sqlstate: "42809".into(),
        message: if definition {
            format!("cannot open relation \"{local_name}\"")
        } else {
            format!("\"{local_name}\" is not a sequence")
        },
    })
}

#[cfg(test)]
mod tests;
