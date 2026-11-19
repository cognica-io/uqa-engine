//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete catalog claims retain exact nodes until ordinary and foreign declarations finish.

use uqa_core::memory::{OwnedSet, ProductionControl};
use uqa_sql::{
    ast::{ColumnDef, ConstraintCatalogIdentity, TableConstraintSet},
    schema::constraint_metadata::identity::foreign_keys,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

#[derive(Default)]
pub(super) struct GlobalClaims {
    objects: OwnedSet<[u8; 16]>,
    oids: OwnedSet<i64>,
}

impl GlobalClaims {
    pub(super) fn register(
        &mut self,
        identity: Option<ConstraintCatalogIdentity>,
        kind: &str,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let Some(identity) = identity else {
            return Ok(());
        };
        let insert = |objects: &mut OwnedSet<_>, value| match control.budget() {
            Some(budget) => objects.insert_budgeted(value, budget),
            None => Ok(objects.insert(value)),
        };
        let object = insert(&mut self.objects, identity.object_id)?;
        let oid = if object {
            match control.budget() {
                Some(budget) => self.oids.insert_budgeted(identity.oid, budget)?,
                None => self.oids.insert(identity.oid),
            }
        } else {
            false
        };
        if !object || !oid {
            return Err(StorageBackendError::Other(format!(
                "duplicate {kind} catalog identity"
            )));
        }
        Ok(())
    }
}

pub(super) fn slots<'a>(
    columns: &'a [ColumnDef],
    constraints: &'a TableConstraintSet,
) -> impl Iterator<Item = (Option<ConstraintCatalogIdentity>, &'static str)> + 'a {
    columns
        .iter()
        .map(|column| (column.not_null_identity, "NOT NULL constraint"))
        .chain(
            foreign_keys::identity_slots(columns, &constraints.foreign_keys).map(|slot| {
                let identity = match slot {
                    foreign_keys::IdentitySlot::Empty => None,
                    foreign_keys::IdentitySlot::Required(identity) => identity,
                };
                (identity, "foreign-key")
            }),
        )
        .chain(
            super::addresses::key_and_check_slots(columns, constraints)
                .map(|identity| (identity, "key or CHECK constraint")),
        )
}
