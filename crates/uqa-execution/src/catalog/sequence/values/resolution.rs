//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve values against one retained definition and authorization view.

use super::{NextvalTarget, SequenceValueContext, SequenceValueError};
use crate::catalog::sequence::snapshot::SequenceReadSnapshot;
use crate::row_locks::{binding::acquire_relation, RelationLockMode};
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

pub(super) struct BoundSequenceValue {
    pub name: String,
    pub object_id: [u8; 16],
}

impl SequenceValueContext<'_> {
    pub(super) fn read_snapshot(&self) -> Result<SequenceReadSnapshot, SequenceValueError> {
        self.snapshots.sequence_read_snapshot().map_err(|error| {
            super::persistence::sequence_storage_error("load sequence catalog", error)
        })
    }

    pub(super) fn bind_sequence_value_reference(
        &self,
        reference: &str,
    ) -> Result<BoundSequenceValue, SequenceValueError> {
        let target = match self
            .privileges
            .resolution
            .visible_relation_kind(reference)?
        {
            RelationResolution::Found(name, "sequence") => ValueReference::Name(name),
            RelationResolution::Found(name, kind) => {
                let relation = RelationIdentity::from_legacy_name(&name)
                    .map_err(SequenceValueError::Internal)?;
                return Err(SequenceValueError::WrongKind {
                    name: relation.name,
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
            ValueReference::Oid(oid) => resolve_oid_reference(&snapshot, oid, |oid| {
                crate::catalog::projection::resolve_regclass_kind_by_oid(&self.catalog, oid)
            })?,
        };
        let name = relation.qualified_name();
        let object_id = snapshot
            .object_ids
            .get(&relation)
            .copied()
            .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        Ok(BoundSequenceValue { name, object_id })
    }

    pub(super) fn lock_sequence_value_target(
        &self,
        bound: &BoundSequenceValue,
        access: ValueAccess,
    ) -> Result<NextvalTarget, SequenceValueError> {
        let mut name = bound.name.clone();
        loop {
            let guard = acquire_relation(self.locks, &name, RelationLockMode::RowExclusive, false)?;
            // Refresh sequence definitions and authority through their independent catalog view without advancing the caller's ordinary SQL data snapshot.
            let snapshot = self.read_snapshot()?;
            let relation = snapshot
                .object_ids
                .iter()
                .find_map(|(relation, object_id)| {
                    (*object_id == bound.object_id).then(|| relation.clone())
                })
                .ok_or_else(|| {
                    SequenceValueError::MissingOid(snapshot.catalog_oid(&bound.object_id))
                })?;
            let current_name = relation.qualified_name();
            if current_name != name {
                name = current_name;
                continue;
            }
            guard.retain_at(self.transactions.transaction_lock_mark());
            return self.sequence_value_target(&snapshot, relation, bound.object_id, access);
        }
    }

    fn sequence_value_target(
        &self,
        snapshot: &SequenceReadSnapshot,
        relation: RelationIdentity,
        object_id: [u8; 16],
        access: ValueAccess,
    ) -> Result<NextvalTarget, SequenceValueError> {
        let name = relation.qualified_name();
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
}

fn resolve_oid_reference(
    snapshot: &SequenceReadSnapshot,
    oid: i64,
    other_relation: impl FnOnce(i64) -> Result<Option<(String, String)>, uqa_sql::SQLError>,
) -> Result<RelationIdentity, SequenceValueError> {
    if let Some(relation) = snapshot.relation_with_oid(oid) {
        return Ok(relation);
    }
    let Some((name, kind)) = other_relation(oid)? else {
        return Err(SequenceValueError::MissingOid(oid));
    };
    let kind = match kind.as_str() {
        "r" => "table",
        "p" => "partitioned table",
        "v" => "view",
        "m" => "materialized view",
        "f" => "foreign table",
        "i" => "index",
        "I" => "partitioned index",
        "t" => "TOAST table",
        "c" => "composite type",
        "S" => return Err(SequenceValueError::MissingOid(oid)),
        _ => {
            return Err(SequenceValueError::Internal(format!(
                "unknown relation kind {kind}"
            )))
        }
    };
    Err(SequenceValueError::WrongKind { name, kind })
}

#[cfg(test)]
mod tests;
