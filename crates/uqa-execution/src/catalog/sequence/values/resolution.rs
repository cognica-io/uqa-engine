//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve values against one retained definition and authorization view.

use super::{NextvalTarget, SequenceValueContext, SequenceValueError};
use crate::catalog::sequence::snapshot::SequenceReadSnapshot;
use uqa_core::RelationIdentity;
use uqa_sql::catalog::resolution::RelationResolution;

pub(super) enum ValueAccess {
    Next,
    Current,
    Set,
}

enum ValueReference {
    Name(String),
    Oid(i64),
}

impl SequenceValueContext<'_> {
    pub(super) fn read_snapshot(&self) -> Result<SequenceReadSnapshot, SequenceValueError> {
        self.snapshots.sequence_read_snapshot().map_err(|error| {
            super::persistence::sequence_storage_error("load sequence catalog", error)
        })
    }

    pub(super) fn resolve_sequence_value_target(
        &self,
        reference: &str,
        access: ValueAccess,
    ) -> Result<NextvalTarget, SequenceValueError> {
        let resolved = self
            .privileges
            .resolution
            .visible_relation_kind(reference)
            .map_err(SequenceValueError::Security)?;
        let target = match resolved {
            RelationResolution::Found(name, "sequence") => ValueReference::Name(name),
            RelationResolution::Found(_, kind) => {
                return Err(SequenceValueError::WrongKind {
                    name: reference.to_string(),
                    kind,
                });
            }
            RelationResolution::MissingRelation | RelationResolution::MissingSchema(_) => {
                ValueReference::Oid(
                    reference
                        .parse::<i64>()
                        .map_err(|_| SequenceValueError::Undefined(reference.into()))?,
                )
            }
        };
        let snapshot = self.read_snapshot()?;
        let relation = match target {
            ValueReference::Name(name) => {
                RelationIdentity::from_legacy_name(&name).map_err(|error| {
                    SequenceValueError::Internal(format!("resolve sequence `{name}`: {error}"))
                })?
            }
            ValueReference::Oid(oid) => snapshot
                .object_ids
                .iter()
                .find_map(|(relation, object_id)| {
                    (crate::catalog::projection::sequence_relation_oid(*object_id) == oid)
                        .then(|| relation.clone())
                })
                .ok_or_else(|| SequenceValueError::Undefined(reference.into()))?,
        };
        let name = relation.qualified_name();
        let object_id = snapshot
            .object_ids
            .get(&relation)
            .copied()
            .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        let state = snapshot
            .sequences
            .get(&relation)
            .copied()
            .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        let temporary = snapshot.persistence.get(&relation)
            == Some(&uqa_sql::ast::RelationPersistence::Temporary);
        let privileges = snapshot.privileges(&self.privileges);
        match access {
            ValueAccess::Next => privileges.ensure_sequence_nextval_privilege(&name, &relation)?,
            ValueAccess::Current => {
                privileges.ensure_sequence_currval_privilege(&name, &relation)?;
            }
            ValueAccess::Set => privileges.ensure_sequence_setval_privilege(&name, &relation)?,
        }
        Ok(NextvalTarget {
            name,
            relation,
            object_id,
            state,
            temporary,
        })
    }

    pub(super) fn resolve_nextval_target(
        &self,
        name: &str,
    ) -> Result<NextvalTarget, SequenceValueError> {
        let target = self.resolve_sequence_value_target(name, ValueAccess::Next)?;
        if self.runtime.current_transaction_is_read_only() && !target.temporary {
            return Err(SequenceValueError::ReadOnly("nextval"));
        }
        Ok(target)
    }
}
