//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` sequence tuple and parameter introspection.

use super::{Engine, RelationIdentity, SQLError, SequenceState, Value};
use crate::engine_state::SequenceSecurity;

#[derive(Clone)]
struct IntrospectionSequence {
    relation: RelationIdentity,
    state: SequenceState,
    security: SequenceSecurity,
    persistence: uqa_sql::ast::RelationPersistence,
}

impl Engine {
    pub(crate) fn pg_sequence_parameters_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        let Some(oid) = strict_sequence_oid("pg_sequence_parameters", arguments)? else {
            return Ok(Value::Null);
        };
        let sequence = match self.sequence_for_introspection(oid)? {
            Some(sequence) => sequence,
            None if crate::sql::resolve_regclass_kind_by_oid(self, oid)?.is_some() => {
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
                    super::SequenceDataType::SmallInt => 21,
                    super::SequenceDataType::Integer => 23,
                    super::SequenceDataType::BigInt => 20,
                }),
            ),
        ]))
    }

    pub(crate) fn pg_get_sequence_data_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        let Some(oid) = self.strict_sequence_regclass_oid("pg_get_sequence_data", arguments)?
        else {
            return Ok(Value::Null);
        };
        let Some(sequence) = self.sequence_for_introspection(oid)? else {
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

    pub(crate) fn pg_sequence_last_value_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        let Some(oid) = self.strict_sequence_regclass_oid("pg_sequence_last_value", arguments)?
        else {
            return Ok(Value::Null);
        };
        let Some(sequence) = self.sequence_for_introspection(oid)? else {
            return if crate::sql::resolve_regclass_kind_by_oid(self, oid)?.is_some() {
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

    fn sequence_for_introspection(
        &self,
        oid: i64,
    ) -> Result<Option<IntrospectionSequence>, SQLError> {
        self.refresh_sequences_from_catalog().map_err(|error| {
            SQLError::Internal(format!("load sequences for introspection: {error}"))
        })?;
        let object_ids = self.durable.sequence_object_ids.read();
        let Some(relation) = object_ids.iter().find_map(|(relation, object_id)| {
            (crate::sql::sequence_relation_oid(*object_id) == oid).then(|| relation.clone())
        }) else {
            return Ok(None);
        };
        let state = self
            .durable
            .sequences
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` disappeared",
                    relation.qualified_name()
                ))
            })?;
        let security = self
            .durable
            .sequence_security
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        let persistence = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .copied()
            .unwrap_or_default();
        Ok(Some(IntrospectionSequence {
            relation,
            state,
            security,
            persistence,
        }))
    }

    fn strict_sequence_regclass_oid(
        &self,
        function: &str,
        arguments: &[Value],
    ) -> Result<Option<i64>, SQLError> {
        let [argument] = arguments else {
            return Err(SQLError::BadArity {
                name: function.into(),
                expected: "1".into(),
                actual: arguments.len(),
            });
        };
        match argument {
            Value::Null => Ok(None),
            Value::Int(oid) => Ok(Some(*oid)),
            Value::Str(name) | Value::FixedChar(name) => {
                crate::sql::resolve_regclass_oid(self, name)
                    .map_err(SQLError::Internal)?
                    .map(Some)
                    .ok_or_else(|| SQLError::Routine {
                        sqlstate: "42P01".into(),
                        message: format!("relation \"{name}\" does not exist"),
                    })
            }
            value => Err(SQLError::TypeMismatch(format!(
                "{function} requires regclass, got {value:?}"
            ))),
        }
    }

    fn sequence_is_from_other_temporary_session(&self, sequence: &IntrospectionSequence) -> bool {
        sequence.persistence == uqa_sql::ast::RelationPersistence::Temporary
            && sequence.relation.schema != self.temporary_schema_name()
    }

    fn sequence_introspection_privilege(
        &self,
        sequence: &IntrospectionSequence,
        access: SequenceAccess,
    ) -> bool {
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        match access {
            SequenceAccess::Any => crate::engine_sequence_security::role_can_view_sequence(
                &sequence.security,
                &current_user,
                &roles,
                &memberships,
            ),
            SequenceAccess::Select => crate::engine_sequence_security::role_can_select_sequence(
                &sequence.security,
                &current_user,
                &roles,
                &memberships,
            ),
            SequenceAccess::ReadValue => {
                crate::engine_sequence_security::role_can_read_sequence_value(
                    &sequence.security,
                    &current_user,
                    &roles,
                    &memberships,
                )
            }
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

fn strict_sequence_oid(function: &str, arguments: &[Value]) -> Result<Option<i64>, SQLError> {
    let [argument] = arguments else {
        return Err(SQLError::BadArity {
            name: function.into(),
            expected: "1".into(),
            actual: arguments.len(),
        });
    };
    match argument {
        Value::Null => Ok(None),
        Value::Int(oid) => Ok(Some(*oid)),
        value => Err(SQLError::TypeMismatch(format!(
            "{function} requires oid or regclass, got {value:?}"
        ))),
    }
}

fn null_sequence_data() -> Value {
    Value::Record(vec![
        ("last_value".into(), Value::Null),
        ("is_called".into(), Value::Null),
    ])
}
