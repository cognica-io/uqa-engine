//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture coherent sequence inquiry authority without publishing partial live catalogs.

use crate::catalog::{
    context::CatalogContext,
    projection::{resolve_regclass_kind_by_oid, sequence_relation_oid},
    sequence::snapshot::{SequenceReadSnapshot, SequenceSnapshotSource},
};
use uqa_core::Value;
use uqa_sql::{
    catalog::security::sequence_inquiry::{
        missing_sequence, SequencePrivilegeArguments, SequencePrivilegeInquiry,
        SequencePrivilegeTarget,
    },
    SQLError,
};

pub struct SequencePrivilegeReadContext<'a> {
    pub inquiry: SequencePrivilegeInquiry<'a>,
    pub snapshots: &'a dyn SequenceSnapshotSource,
    pub catalog: CatalogContext<'a>,
}

impl SequencePrivilegeReadContext<'_> {
    pub fn has_sequence_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        sequence_privilege_value(&self.inquiry, self.snapshots, arguments, |oid| {
            resolve_regclass_kind_by_oid(&self.catalog, oid)
        })
    }
}

fn sequence_privilege_value(
    inquiry: &SequencePrivilegeInquiry<'_>,
    snapshots: &dyn SequenceSnapshotSource,
    arguments: &[Value],
    other_relation: impl FnOnce(i64) -> Result<Option<(String, String)>, SQLError>,
) -> Result<Value, SQLError> {
    let Some(arguments) = SequencePrivilegeArguments::parse(arguments)? else {
        return Ok(Value::Null);
    };
    // Explicit role lookup precedes privilege validation and binds an incarnation before any later name-resolution refresh.
    let role_snapshot = arguments
        .has_explicit_subject()
        .then(|| read_snapshot(snapshots))
        .transpose()?;
    let roles = role_snapshot
        .as_ref()
        .map_or(inquiry.roles, |snapshot| snapshot);
    let request = arguments.bind(inquiry.names, roles)?;
    let (relation, snapshot) = match request.target {
        SequencePrivilegeTarget::Name(reference) => {
            let (_, relation) = request
                .target
                .resolve(inquiry.resolution)?
                .ok_or_else(|| missing_sequence(reference))?;
            let snapshot = read_snapshot(snapshots)?;
            if !snapshot.object_ids.contains_key(&relation) {
                return Err(missing_sequence(reference));
            }
            (relation, snapshot)
        }
        SequencePrivilegeTarget::Oid(oid) => {
            let snapshot = match role_snapshot {
                Some(snapshot) => snapshot,
                None => read_snapshot(snapshots)?,
            };
            let relation = snapshot
                .object_ids
                .iter()
                .find_map(|(relation, object_id)| {
                    (sequence_relation_oid(*object_id) == oid).then(|| relation.clone())
                });
            let Some(relation) = relation else {
                if let Some((name, kind)) = other_relation(oid)? {
                    // An older statement catalog can still contain a sequence removed from the current authority view.
                    if kind != "S" {
                        return Err(SQLError::Routine {
                            sqlstate: "42809".into(),
                            message: format!("\"{name}\" is not a sequence"),
                        });
                    }
                }
                return Ok(Value::Null);
            };
            (relation, snapshot)
        }
    };
    request.evaluate(&relation, &snapshot, &snapshot)
}

fn read_snapshot(snapshots: &dyn SequenceSnapshotSource) -> Result<SequenceReadSnapshot, SQLError> {
    snapshots
        .sequence_read_snapshot()
        .map_err(|error| SQLError::Internal(format!("load sequence privilege catalog: {error}")))
}

#[cfg(test)]
mod tests;
