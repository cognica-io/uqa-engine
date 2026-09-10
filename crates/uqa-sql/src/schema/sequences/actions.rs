//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate combinations of SQL sequence actions and persistence changes.
use crate::ast::{RelationPersistence, SequenceBound, SequenceRestart};
use crate::SQLError;

pub fn altered_sequence_persistence(
    alter: &crate::ast::AlterSequence,
    current: RelationPersistence,
    relation_name: &str,
) -> Result<RelationPersistence, SQLError> {
    match alter.persistence {
        None => Ok(current),
        Some(RelationPersistence::Permanent | RelationPersistence::Unlogged)
            if current == RelationPersistence::Temporary =>
        {
            Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!(
                    "cannot change logged status of table \"{relation_name}\" because it is temporary"
                ),
            })
        }
        Some(requested @ (RelationPersistence::Permanent | RelationPersistence::Unlogged)) => {
            Ok(requested)
        }
        Some(RelationPersistence::Temporary) => Err(SQLError::Internal(
            "ALTER SEQUENCE cannot request temporary persistence".into(),
        )),
    }
}

pub fn sequence_alter_is_persistence_only(alter: &crate::ast::AlterSequence) -> bool {
    alter.persistence.is_some()
        && alter.role_owner.is_none()
        && alter.restart == SequenceRestart::Unchanged
        && alter.increment.is_none()
        && alter.start.is_none()
        && alter.data_type.is_none()
        && alter.min_value == SequenceBound::Unchanged
        && alter.max_value == SequenceBound::Unchanged
        && alter.cycle.is_none()
        && alter.cache_size.is_none()
        && alter.ownership == crate::ast::SequenceOwnership::Unchanged
}

pub fn validate_sequence_role_owner_shape(
    alter: &crate::ast::AlterSequence,
) -> Result<(), SQLError> {
    if alter.restart != SequenceRestart::Unchanged
        || alter.increment.is_some()
        || alter.start.is_some()
        || alter.data_type.is_some()
        || alter.min_value != SequenceBound::Unchanged
        || alter.max_value != SequenceBound::Unchanged
        || alter.cycle.is_some()
        || alter.cache_size.is_some()
        || alter.ownership != crate::ast::SequenceOwnership::Unchanged
        || alter.persistence.is_some()
        || alter.lifecycle != crate::ast::SequenceLifecycle::Unchanged
    {
        return Err(SQLError::Internal(
            "ALTER SEQUENCE OWNER TO cannot contain another action".into(),
        ));
    }
    Ok(())
}
