//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain a constraint destination within its immutable owning relation.

pub mod catalog;
#[cfg(test)]
mod tests;
use crate::row_locks::{
    shared_objects::{SharedCatalogLock, SharedObjectLockSession},
    RelationLockMode,
};
use catalog::{ConstraintNameCatalog, ConstraintTableMetadata};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, TableConstraintSet},
    schema::{
        constraint_changes::names::ConstraintNames,
        constraint_metadata::{CatalogOidClass, ConstraintNameScope},
    },
    SQLError,
};

#[derive(Clone, Copy)]
pub struct ConstraintNameContext<'a> {
    pub catalog: &'a dyn ConstraintNameCatalog,
    pub locks: &'a dyn SharedObjectLockSession,
}

fn trigger_names<'a>(
    triggers: &'a uqa_sql::catalog::events::TriggerCatalog,
    relation: &RelationIdentity,
) -> impl Iterator<Item = &'a str> {
    triggers
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
    catalog: &dyn ConstraintNameCatalog,
    relation: &RelationIdentity,
) -> BTreeSet<String> {
    let table = catalog.constraint_table(relation);
    let triggers = catalog.constraint_triggers();
    table
        .as_ref()
        .into_iter()
        .flat_map(|table| table.names().entries().map(|entry| entry.name))
        .chain(trigger_names(&triggers, relation))
        .map(str::to_owned)
        .collect()
}

pub(crate) fn event_names(
    catalog: &dyn ConstraintNameCatalog,
    relation: &RelationIdentity,
) -> BTreeSet<String> {
    trigger_names(&catalog.constraint_triggers(), relation)
        .map(str::to_owned)
        .collect()
}

pub(crate) fn schema_names(catalog: &dyn ConstraintNameCatalog, schema: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for table in catalog.constraint_schema_tables(schema) {
        names.extend(table.names().entries().map(|entry| entry.name.to_owned()));
    }
    for (relation, table) in catalog.constraint_foreign_tables().iter() {
        if relation.schema == schema {
            names.extend(
                ConstraintNames {
                    columns: &table.columns,
                    checks: &table.checks,
                    foreign_keys: &[],
                    keys: &[],
                }
                .entries()
                .map(|entry| entry.name.to_owned()),
            );
        }
    }
    let triggers = catalog.constraint_triggers();
    for relation in triggers.keys().filter(|relation| relation.schema == schema) {
        names.extend(trigger_names(&triggers, relation).map(str::to_owned));
    }
    for domain in catalog
        .constraint_domains()
        .values()
        .filter(|domain| domain.identity.schema == schema)
    {
        names.extend(
            domain
                .definition
                .checks
                .iter()
                .filter_map(|check| check.name.clone()),
        );
        names.extend(
            domain
                .definition
                .not_null
                .as_ref()
                .and_then(|constraint| constraint.name.clone()),
        );
    }
    names
}

pub(crate) fn name_scope(
    catalog: &dyn ConstraintNameCatalog,
    relation: &RelationIdentity,
) -> ConstraintNameScope {
    ConstraintNameScope {
        events: event_names(catalog, relation),
        schema: schema_names(catalog, &relation.schema),
    }
}

fn owner(
    catalog: &dyn ConstraintNameCatalog,
    object_id: [u8; 16],
) -> Result<(RelationIdentity, ConstraintTableMetadata), SQLError> {
    catalog
        .constraint_owner(object_id)
        .ok_or_else(|| SQLError::Internal("constraint owning relation disappeared".into()))
}

impl ConstraintNameContext<'_> {
    pub fn existing_names(&self, table: &str) -> Result<BTreeSet<String>, SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        Ok(existing_names(self.catalog, &relation))
    }

    pub fn name_scope(&self, relation: &RelationIdentity) -> ConstraintNameScope {
        name_scope(self.catalog, relation)
    }

    pub fn automatic_names(&self, table: &str) -> Result<BTreeSet<String>, SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        Ok(schema_names(self.catalog, &relation.schema))
    }
    pub fn bind(&self, table: &str) -> Result<[u8; 16], SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        self.catalog
            .constraint_table(&relation)
            .map(|table| table.object_id)
            .ok_or_else(|| SQLError::UnknownTable(table.into()))
    }

    pub fn in_use(&self, object_id: [u8; 16], name: &str) -> Result<bool, SQLError> {
        let (relation, table) = owner(self.catalog, object_id)?;
        Ok(table
            .names()
            .entries()
            .any(|constraint| constraint.name == name)
            || trigger_names(&self.catalog.constraint_triggers(), &relation)
                .any(|candidate| candidate == name))
    }

    pub fn ensure_available(&self, table: &str, name: Option<&str>) -> Result<(), SQLError> {
        if let Some(name) = name {
            if self.in_use(self.bind(table)?, name)? {
                let relation =
                    RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
                return Err(super::constraint_error(
                    "42710",
                    format!(
                        "constraint \"{name}\" for relation \"{}\" already exists",
                        relation.name
                    ),
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
        let (_, table) = owner(self.catalog, object_id)?;
        crate::schema::indexes::constraint_names::rebind_key_names(
            constraints.key_constraints.iter_mut().chain(
                constraints
                    .hierarchy
                    .partition_inherited_key_constraints
                    .iter_mut(),
            ),
            table.key_names(),
        );
        let before = table
            .names()
            .entries()
            .map(|entry| (entry.name, entry.object_id))
            .collect::<std::collections::BTreeMap<_, _>>();
        for entry in ConstraintNames::from_definition(columns, constraints).entries() {
            if before.get(entry.name) != Some(&entry.object_id) {
                self.reserve(object_id, entry.name)?;
            }
        }
        self.rebind_key_names(object_id, constraints)
    }

    /// Retain independent index renames that became visible during a name reservation.
    pub(crate) fn rebind_key_names(
        &self,
        object_id: [u8; 16],
        constraints: &mut TableConstraintSet,
    ) -> Result<(), SQLError> {
        let (_, table) = owner(self.catalog, object_id)?;
        crate::schema::indexes::constraint_names::rebind_key_names(
            constraints.key_constraints.iter_mut().chain(
                constraints
                    .hierarchy
                    .partition_inherited_key_constraints
                    .iter_mut(),
            ),
            table.key_names(),
        );
        Ok(())
    }
}
