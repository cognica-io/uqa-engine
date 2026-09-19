//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve the public addresses of predecessor key and CHECK rows during initial conversion.

use super::{ConstraintMetadataError, ConstraintMetadataResult};
use crate::ast::{ColumnDef, TableConstraintSet};
use uqa_core::RelationIdentity;

pub struct LegacyIdentities {
    keys: Vec<bool>,
    column_checks: Vec<Option<LegacyCheck>>,
    checks: Vec<Option<LegacyCheck>>,
}

#[derive(Clone, Copy)]
enum LegacyCheck {
    Named,
    Identified([u8; 16]),
}

impl LegacyCheck {
    fn from_object(object: Option<[u8; 16]>) -> Self {
        object.map_or(Self::Named, Self::Identified)
    }

    fn object_id(self) -> Option<[u8; 16]> {
        match self {
            Self::Named => None,
            Self::Identified(object) => Some(object),
        }
    }
}

impl LegacyIdentities {
    pub fn capture(columns: &[ColumnDef], constraints: &TableConstraintSet) -> Self {
        Self {
            keys: constraints
                .key_constraints
                .iter()
                .map(|key| key.catalog_identity.is_none())
                .collect(),
            column_checks: columns
                .iter()
                .map(|column| {
                    (column.check.is_some() && column.check_catalog_oid.is_none())
                        .then(|| LegacyCheck::from_object(column.check_object_id))
                })
                .collect(),
            checks: constraints
                .checks
                .iter()
                .map(|check| {
                    check
                        .catalog_oid
                        .is_none()
                        .then(|| LegacyCheck::from_object(check.object_id))
                })
                .collect(),
        }
    }

    /// Return the newly converted incarnations whose legacy address can be reassigned if another catalog row already owns it.
    pub fn preserve_oids(
        &self,
        relation: &RelationIdentity,
        columns: &mut [ColumnDef],
        constraints: &mut TableConstraintSet,
    ) -> ConstraintMetadataResult<Vec<[u8; 16]>> {
        let mut converted = Vec::new();
        for (index, key) in constraints.key_constraints.iter_mut().enumerate() {
            if self.keys.get(index).copied().unwrap_or(true) {
                let identity = key
                    .catalog_identity
                    .as_mut()
                    .ok_or_else(|| invalid("migrated key has no catalog identity"))?;
                identity.oid = legacy_oid(relation, key.name.as_deref(), None)?;
                converted.push(identity.object_id);
            }
        }
        for (column, old) in columns.iter_mut().zip(&self.column_checks) {
            if let Some(legacy) = old {
                column.check_catalog_oid = Some(legacy_oid(
                    relation,
                    column.check_name.as_deref(),
                    legacy.object_id(),
                )?);
                converted.push(
                    column
                        .check_object_id
                        .ok_or_else(|| invalid("migrated CHECK has no incarnation"))?,
                );
            }
        }
        for (check, old) in constraints.checks.iter_mut().zip(&self.checks) {
            if let Some(legacy) = old {
                check.catalog_oid = Some(legacy_oid(
                    relation,
                    check.name.as_deref(),
                    legacy.object_id(),
                )?);
                converted.push(
                    check
                        .object_id
                        .ok_or_else(|| invalid("migrated CHECK has no incarnation"))?,
                );
            }
        }
        super::keys::synchronize_provenance(constraints);
        Ok(converted)
    }
}

fn legacy_oid(
    relation: &RelationIdentity,
    name: Option<&str>,
    object: Option<[u8; 16]>,
) -> ConstraintMetadataResult<i64> {
    if let Some(object) = object {
        return Ok(crate::catalog::oids::stable_object_oid(
            "constraint",
            &object,
        ));
    }
    let name = name.ok_or_else(|| invalid("migrated constraint has no name"))?;
    Ok(crate::catalog::oids::stable_oid(
        "constraint",
        &format!("{}.{}.{name}", relation.schema, relation.name),
    ))
}

fn invalid(message: &str) -> ConstraintMetadataError {
    ConstraintMetadataError::Invalid(message.into())
}
