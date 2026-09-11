//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live sequence introspection, authorization, temporary-session visibility, and result records.

use super::{
    context::CatalogContext,
    projection::{resolve_regclass_kind_by_oid, resolve_regclass_oid, sequence_relation_oid},
    security::{roles::RoleCatalogGuards, SequenceSecurity},
    sequence::SequenceState,
};
use std::collections::BTreeMap;
use uqa_core::{RelationIdentity, Value};
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    catalog::sequence_functions::{
        serial_sequence_owner, strict_sequence_oid, strict_sequence_regclass_oid,
    },
    schema::sequences::ownership::SequenceOwnerCatalog,
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub type SequenceObjectIdsRead<'a> =
    Box<dyn std::ops::Deref<Target = BTreeMap<RelationIdentity, [u8; 16]>> + 'a>;
pub type SequenceStatesRead<'a> =
    Box<dyn std::ops::Deref<Target = BTreeMap<RelationIdentity, SequenceState>> + 'a>;

/// Separate reads retain the object-id guard through state, security and persistence lookup.
pub trait SequenceIntrospectionCatalog {
    fn refresh_sequences(&self) -> StorageBackendResult<()>;
    fn object_ids(&self) -> SequenceObjectIdsRead<'_>;
    fn states(&self) -> SequenceStatesRead<'_>;
    fn sequence_state(&self, relation: &RelationIdentity) -> Option<SequenceState>;
    fn sequence_security(&self, relation: &RelationIdentity) -> Option<SequenceSecurity>;
    fn sequence_persistence(&self, relation: &RelationIdentity) -> Option<RelationPersistence>;
}

pub struct SequenceIntrospectionContext<'a> {
    pub catalog: CatalogContext<'a>,
    pub sequences: &'a dyn SequenceIntrospectionCatalog,
    pub owners: &'a dyn SequenceOwnerCatalog,
    pub roles: &'a dyn RoleCatalogGuards,
}

#[derive(Clone)]
struct IntrospectionSequence {
    relation: RelationIdentity,
    state: SequenceState,
    security: SequenceSecurity,
    persistence: RelationPersistence,
}

impl SequenceIntrospectionContext<'_> {
    pub fn pg_get_serial_sequence_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        let Some(owner_column) = serial_sequence_owner(self.owners, arguments)? else {
            return Ok(Value::Null);
        };
        self.sequences.refresh_sequences().map_err(|error| {
            SQLError::Internal(format!("load sequence ownership catalog: {error}"))
        })?;
        let sequence = self
            .sequences
            .states()
            .iter()
            .find(|(_, state)| {
                state.owner.is_some_and(|owner| {
                    owner.table_object_id == owner_column.table_object_id
                        && owner.column_object_id == owner_column.column_object_id
                })
            })
            .map(|(relation, _)| relation.qualified_name());
        Ok(sequence.map_or(Value::Null, Value::Str))
    }

    pub fn pg_sequence_parameters_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        let Some(oid) = strict_sequence_oid("pg_sequence_parameters", arguments)? else {
            return Ok(Value::Null);
        };
        let sequence = match read_sequence(self.sequences, oid)? {
            Some(sequence) => sequence,
            None if resolve_regclass_kind_by_oid(&self.catalog, oid)?.is_some() => {
                return Err(SQLError::Routine {
                    sqlstate: "XX000".into(),
                    message: format!("cache lookup failed for sequence {oid}"),
                });
            }
            None => {
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation with OID {oid} does not exist"),
                });
            }
        };
        self.ensure_sequence_introspection_privilege(&sequence, SequenceAccess::Any)?;
        let state = sequence.state;
        Ok(Value::Record(vec![
            ("start_value".into(), Value::Int(state.start)),
            ("minimum_value".into(), Value::Int(state.min_value)),
            ("maximum_value".into(), Value::Int(state.max_value)),
            ("increment".into(), Value::Int(state.increment)),
            ("cycle_option".into(), Value::Bool(state.cycle)),
            ("cache_size".into(), Value::Int(state.cache_size)),
            (
                "data_type".into(),
                Value::Int(match state.data_type {
                    SequenceDataType::SmallInt => 21,
                    SequenceDataType::Integer => 23,
                    SequenceDataType::BigInt => 20,
                }),
            ),
        ]))
    }

    pub fn pg_get_sequence_data_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        let Some(oid) = self.strict_sequence_regclass_oid("pg_get_sequence_data", arguments)?
        else {
            return Ok(Value::Null);
        };
        let Some(sequence) = read_sequence(self.sequences, oid)? else {
            return Ok(null_sequence_data());
        };
        if self.sequence_is_from_other_temporary_session(&sequence)
            || !self.sequence_introspection_privilege(&sequence, SequenceAccess::Select)
        {
            return Ok(null_sequence_data());
        }
        Ok(Value::Record(vec![
            ("last_value".into(), Value::Int(sequence.state.current)),
            ("is_called".into(), Value::Bool(sequence.state.called)),
        ]))
    }

    pub fn pg_sequence_last_value_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        let Some(oid) = self.strict_sequence_regclass_oid("pg_sequence_last_value", arguments)?
        else {
            return Ok(Value::Null);
        };
        let Some(sequence) = read_sequence(self.sequences, oid)? else {
            return if resolve_regclass_kind_by_oid(&self.catalog, oid)?.is_some() {
                Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("cannot open relation with OID {oid}"),
                })
            } else {
                Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("could not open relation with OID {oid}"),
                })
            };
        };
        if self.sequence_is_from_other_temporary_session(&sequence)
            || !self.sequence_introspection_privilege(&sequence, SequenceAccess::ReadValue)
            || !sequence.state.called
        {
            return Ok(Value::Null);
        }
        Ok(Value::Int(sequence.state.current))
    }

    fn strict_sequence_regclass_oid(
        &self,
        function: &str,
        arguments: &[Value],
    ) -> Result<Option<i64>, SQLError> {
        strict_sequence_regclass_oid(function, arguments, &mut |name| {
            resolve_regclass_oid(&self.catalog, name)
        })
    }

    fn sequence_is_from_other_temporary_session(&self, sequence: &IntrospectionSequence) -> bool {
        sequence.persistence == RelationPersistence::Temporary
            && sequence.relation.schema != self.catalog.session.temporary_schema_name()
    }

    fn sequence_introspection_privilege(
        &self,
        sequence: &IntrospectionSequence,
        access: SequenceAccess,
    ) -> bool {
        let current_user = self.catalog.current_user_name();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        match access {
            SequenceAccess::Any => super::security::sequence::role_can_view_sequence(
                &sequence.security,
                &current_user,
                &roles,
                &memberships,
            ),
            SequenceAccess::Select => super::security::sequence::role_can_select_sequence(
                &sequence.security,
                &current_user,
                &roles,
                &memberships,
            ),
            SequenceAccess::ReadValue => super::security::sequence::role_can_read_sequence_value(
                &sequence.security,
                &current_user,
                &roles,
                &memberships,
            ),
        }
    }

    fn ensure_sequence_introspection_privilege(
        &self,
        sequence: &IntrospectionSequence,
        access: SequenceAccess,
    ) -> Result<(), SQLError> {
        if self.sequence_introspection_privilege(sequence, access) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for sequence {}", sequence.relation.name),
        })
    }
}

#[derive(Clone, Copy)]
enum SequenceAccess {
    Any,
    Select,
    ReadValue,
}

fn null_sequence_data() -> Value {
    Value::Record(vec![
        ("last_value".into(), Value::Null),
        ("is_called".into(), Value::Null),
    ])
}

fn read_sequence(
    catalog: &dyn SequenceIntrospectionCatalog,
    oid: i64,
) -> Result<Option<IntrospectionSequence>, SQLError> {
    catalog.refresh_sequences().map_err(|error| {
        SQLError::Internal(format!("load sequences for introspection: {error}"))
    })?;
    let object_ids = catalog.object_ids();
    let Some(relation) = object_ids.iter().find_map(|(relation, object_id)| {
        (sequence_relation_oid(*object_id) == oid).then(|| relation.clone())
    }) else {
        return Ok(None);
    };
    let state = catalog.sequence_state(&relation).ok_or_else(|| {
        SQLError::Internal(format!(
            "sequence `{}` disappeared",
            relation.qualified_name()
        ))
    })?;
    let security = catalog.sequence_security(&relation).ok_or_else(|| {
        SQLError::Internal(format!(
            "sequence `{}` has no security metadata",
            relation.qualified_name()
        ))
    })?;
    let persistence = catalog.sequence_persistence(&relation).unwrap_or_default();
    Ok(Some(IntrospectionSequence {
        relation,
        state,
        security,
        persistence,
    }))
}

#[cfg(test)]
mod tests;
