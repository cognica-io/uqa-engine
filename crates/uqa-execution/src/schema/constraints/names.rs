//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain a constraint destination within its immutable owning relation.

use crate::catalog::{services::CatalogSnapshotSource, CatalogReadView, CatalogTableSnapshot};
use crate::row_locks::{
    shared_objects::{SharedCatalogLock, SharedObjectLockSession},
    RelationLockMode,
};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, TableConstraintSet},
    schema::{constraint_changes::names::ConstraintNames, constraint_metadata::CatalogOidClass},
    SQLError,
};

#[derive(Clone, Copy)]
pub struct ConstraintNameContext<'a> {
    pub catalog: &'a dyn CatalogSnapshotSource,
    pub locks: &'a dyn SharedObjectLockSession,
}

fn table_names(table: &CatalogTableSnapshot) -> ConstraintNames<'_> {
    ConstraintNames {
        columns: &table.columns,
        checks: &table.checks,
        foreign_keys: &table.foreign_keys,
        keys: &table.keys,
    }
}

fn trigger_names<'a>(
    catalog: &'a CatalogReadView,
    relation: &RelationIdentity,
) -> impl Iterator<Item = &'a str> {
    catalog
        .snapshot()
        .definitions
        .triggers
        .get(relation)
        .into_iter()
        .flat_map(|triggers| triggers.values())
        .filter(|trigger| trigger.definition.constraint)
        .map(|trigger| {
            trigger
                .constraint_name
                .as_deref()
                .unwrap_or(&trigger.definition.name)
        })
}

pub(crate) fn existing_names(
    catalog: &CatalogReadView,
    relation: &RelationIdentity,
) -> BTreeSet<String> {
    catalog
        .snapshot()
        .tables
        .get(relation)
        .into_iter()
        .flat_map(|table| table_names(table).entries().map(|entry| entry.name))
        .chain(trigger_names(catalog, relation))
        .map(str::to_owned)
        .collect()
}

pub(crate) fn event_names(
    catalog: &CatalogReadView,
    relation: &RelationIdentity,
) -> BTreeSet<String> {
    trigger_names(catalog, relation)
        .map(str::to_owned)
        .collect()
}

fn owner(
    catalog: &CatalogReadView,
    object_id: [u8; 16],
) -> Result<(&RelationIdentity, &CatalogTableSnapshot), SQLError> {
    catalog
        .snapshot()
        .tables
        .iter()
        .find(|(_, table)| table.object_id == object_id)
        .ok_or_else(|| SQLError::Internal("constraint owning relation disappeared".into()))
}

impl ConstraintNameContext<'_> {
    pub fn existing_names(&self, table: &str) -> Result<BTreeSet<String>, SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        Ok(existing_names(
            &self.catalog.current_catalog_snapshot(),
            &relation,
        ))
    }

    pub fn trigger_names(&self, relation: &RelationIdentity) -> BTreeSet<String> {
        event_names(&self.catalog.current_catalog_snapshot(), relation)
    }
    pub fn bind(&self, table: &str) -> Result<[u8; 16], SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        self.catalog
            .current_catalog_snapshot()
            .snapshot()
            .tables
            .get(&relation)
            .map(|table| table.object_id)
            .ok_or_else(|| SQLError::UnknownTable(table.into()))
    }

    pub fn in_use(&self, object_id: [u8; 16], name: &str) -> Result<bool, SQLError> {
        let catalog = self.catalog.current_catalog_snapshot();
        let (relation, table) = owner(&catalog, object_id)?;
        Ok(table_names(table)
            .entries()
            .any(|constraint| constraint.name == name)
            || trigger_names(&catalog, relation).any(|candidate| candidate == name))
    }

    pub fn ensure_available(&self, table: &str, name: Option<&str>) -> Result<(), SQLError> {
        if let Some(name) = name {
            if self.in_use(self.bind(table)?, name)? {
                return Err(super::constraint_error(
                    "42710",
                    format!("constraint \"{name}\" for relation \"{table}\" already exists"),
                ));
            }
        }
        Ok(())
    }

    pub fn reserve(&self, object_id: [u8; 16], name: &str) -> Result<(), SQLError> {
        let guard = self.locks.acquire_shared_catalog(
            SharedCatalogLock::MemberName {
                class_id: CatalogOidClass::Constraint.class_id(),
                owner_class_id: CatalogOidClass::Relation.class_id(),
                owner_object_id: object_id,
                name,
            },
            RelationLockMode::AccessExclusive,
        )?;
        self.locks.refresh_shared_catalog()?;
        if self.in_use(object_id, name)? {
            return Err(SQLError::Routine {
                sqlstate: "23505".into(),
                message: "duplicate key value violates unique constraint \"pg_constraint_conrelid_contypid_conname_index\"".into(),
            });
        }
        guard.retain();
        Ok(())
    }

    pub fn reserve_changes(
        &self,
        object_id: [u8; 16],
        columns: &[ColumnDef],
        constraints: &mut TableConstraintSet,
    ) -> Result<(), SQLError> {
        let catalog = self.catalog.current_catalog_snapshot();
        let (_, table) = owner(&catalog, object_id)?;
        crate::schema::indexes::constraint_names::rebind_current_key_names(
            constraints.key_constraints.iter_mut().chain(
                constraints
                    .hierarchy
                    .partition_inherited_key_constraints
                    .iter_mut(),
            ),
            table,
        );
        let before = table_names(table)
            .entries()
            .map(|entry| (entry.name, entry.object_id))
            .collect::<std::collections::BTreeMap<_, _>>();
        for entry in ConstraintNames::from_definition(columns, constraints).entries() {
            if before.get(entry.name) != Some(&entry.object_id) {
                self.reserve(object_id, entry.name)?;
            }
        }
        let refreshed = self.catalog.current_catalog_snapshot();
        let (_, table) = owner(&refreshed, object_id)?;
        crate::schema::indexes::constraint_names::rebind_current_key_names(
            constraints.key_constraints.iter_mut().chain(
                constraints
                    .hierarchy
                    .partition_inherited_key_constraints
                    .iter_mut(),
            ),
            table,
        );
        Ok(())
    }
}
