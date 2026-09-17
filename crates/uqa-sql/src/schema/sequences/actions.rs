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
        && !sequence_alter_changes_value_parameters(alter)
        && alter.ownership == crate::ast::SequenceOwnership::Unchanged
}

/// Explicit value-generation options replace the sequence's allocation generation even when their values are unchanged. Ownership, name and persistence actions are handled separately.
pub fn sequence_alter_changes_value_parameters(alter: &crate::ast::AlterSequence) -> bool {
    alter.restart != SequenceRestart::Unchanged
        || alter.increment.is_some()
        || alter.start.is_some()
        || alter.data_type.is_some()
        || alter.min_value != SequenceBound::Unchanged
        || alter.max_value != SequenceBound::Unchanged
        || alter.cycle.is_some()
        || alter.cache_size.is_some()
}

pub fn validate_sequence_role_owner_shape(
    alter: &crate::ast::AlterSequence,
) -> Result<(), SQLError> {
    if sequence_alter_changes_value_parameters(alter)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_value_generation_distinguishes_explicit_options_from_owner_and_name_changes() {
        for (suffix, expected) in [
            ("AS bigint", true),
            ("INCREMENT BY 1", true),
            ("START WITH 1", true),
            ("RESTART", true),
            ("RESTART WITH 1", true),
            ("MINVALUE 1", true),
            ("NO MINVALUE", true),
            ("MAXVALUE 10", true),
            ("NO MAXVALUE", true),
            ("CYCLE", true),
            ("NO CYCLE", true),
            ("CACHE 1", true),
            ("OWNED BY items.id CACHE 3", true),
            ("OWNED BY NONE", false),
            ("OWNED BY items.id", false),
            ("OWNER TO owner", false),
            ("RENAME TO renamed", false),
            ("SET SCHEMA archive", false),
            ("SET LOGGED", false),
        ] {
            let sql = format!("ALTER SEQUENCE ids {suffix}");
            let crate::Statement::AlterSequence(alter) = crate::compile(&sql).unwrap().remove(0)
            else {
                panic!("expected an ALTER SEQUENCE statement: {sql}");
            };
            assert_eq!(
                sequence_alter_changes_value_parameters(&alter),
                expected,
                "{sql}"
            );
        }
    }
}
