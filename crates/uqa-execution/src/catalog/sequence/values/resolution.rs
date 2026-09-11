//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind sequence value targets with the original name/OID and access-check order.
use super::{NextvalTarget, SequenceValueContext, SequenceValueError};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::resolution::RelationResolution;
impl SequenceValueContext<'_> {
    pub(super) fn resolve_sequence_value_target(
        &self,
        reference: &str,
    ) -> Result<(String, RelationIdentity, [u8; 16]), SequenceValueError> {
        let resolved = self
            .privileges
            .resolution
            .visible_relation_kind(reference)
            .map_err(SequenceValueError::Security)?;
        let (name, kind) = match resolved {
            RelationResolution::Found(name, kind) => (name, kind),
            RelationResolution::MissingRelation | RelationResolution::MissingSchema(_) => {
                return self
                    .resolve_sequence_value_target_by_oid(reference)?
                    .ok_or_else(|| SequenceValueError::Undefined(reference.to_string()));
            }
        };
        if kind != "sequence" {
            return Err(SequenceValueError::WrongKind {
                name: reference.to_string(),
                kind,
            });
        }
        let relation = RelationIdentity::from_legacy_name(&name)
            .map_err(uqa_storage::StorageBackendError::Other)
            .map_err(|error| {
                SequenceValueError::Internal(format!("resolve sequence `{name}`: {error}"))
            })?;
        let object_id = self
            .sequences
            .object_ids()
            .get(&relation)
            .copied()
            .ok_or_else(|| {
                SequenceValueError::Internal(format!(
                    "sequence `{name}` has no durable object identity"
                ))
            })?;
        Ok((name, relation, object_id))
    }
    pub(super) fn resolve_sequence_value_target_by_oid(
        &self,
        reference: &str,
    ) -> Result<Option<(String, RelationIdentity, [u8; 16])>, SequenceValueError> {
        let Ok(oid) = reference.parse::<i64>() else {
            return Ok(None);
        };
        self.sequences.refresh_sequences().map_err(|error| {
            SequenceValueError::Internal(format!("load sequence catalog: {error}"))
        })?;
        let object_ids = self.sequences.object_ids();
        Ok(object_ids.iter().find_map(|(relation, object_id)| {
            (crate::catalog::projection::sequence_relation_oid(*object_id) == oid)
                .then(|| (relation.qualified_name(), relation.clone(), *object_id))
        }))
    }
    pub(super) fn resolve_nextval_target(
        &self,
        name: &str,
    ) -> Result<NextvalTarget, SequenceValueError> {
        let (name, relation, object_id) = self.resolve_sequence_value_target(name)?;
        let state = self
            .sequences
            .states()
            .get(&relation)
            .copied()
            .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        let temporary = self
            .runtime
            .persistence()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        self.privileges
            .ensure_sequence_nextval_privilege(&name, &relation)?;
        if self.runtime.current_transaction_is_read_only() && !temporary {
            return Err(SequenceValueError::ReadOnly("nextval"));
        }
        Ok(NextvalTarget {
            name,
            relation,
            object_id,
            state,
            temporary,
        })
    }
}
