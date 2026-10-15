//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current constraint declarations selected independently of a retained query catalog.

use crate::catalog::{
    domain::DomainRegistry, foreign::StoredForeignTable, CatalogReadSnapshot, CatalogTableSnapshot,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableHierarchy, TableKeyConstraint},
    catalog::events::TriggerCatalog,
    schema::constraint_changes::names::ConstraintNames,
};

pub struct ConstraintTableMetadata {
    pub object_id: [u8; 16],
    pub columns: Arc<Vec<ColumnDef>>,
    pub checks: Arc<Vec<TableCheck>>,
    pub foreign_keys: Arc<Vec<ForeignKey>>,
    pub keys: Arc<Vec<TableKeyConstraint>>,
    pub hierarchy: Arc<TableHierarchy>,
}

impl ConstraintTableMetadata {
    pub(super) fn names(&self) -> ConstraintNames<'_> {
        ConstraintNames {
            columns: &self.columns,
            checks: &self.checks,
            foreign_keys: &self.foreign_keys,
            keys: &self.keys,
        }
    }

    pub(super) fn key_names(&self) -> impl Iterator<Item = &TableKeyConstraint> + Clone {
        self.keys
            .iter()
            .chain(&self.hierarchy.partition_inherited_key_constraints)
    }
}

impl From<&CatalogTableSnapshot> for ConstraintTableMetadata {
    fn from(table: &CatalogTableSnapshot) -> Self {
        Self {
            object_id: table.object_id,
            columns: Arc::clone(&table.columns),
            checks: Arc::clone(&table.checks),
            foreign_keys: Arc::clone(&table.foreign_keys),
            keys: Arc::clone(&table.keys),
            hierarchy: Arc::clone(&table.hierarchy),
        }
    }
}

pub trait ConstraintNameCatalog {
    fn constraint_table(&self, relation: &RelationIdentity) -> Option<ConstraintTableMetadata>;
    fn constraint_owner(
        &self,
        object_id: [u8; 16],
    ) -> Option<(RelationIdentity, ConstraintTableMetadata)>;
    fn constraint_schema_tables(&self, schema: &str) -> Vec<ConstraintTableMetadata>;
    fn constraint_foreign_tables(&self) -> Arc<BTreeMap<RelationIdentity, StoredForeignTable>>;
    fn constraint_domains(&self) -> Arc<DomainRegistry>;
    fn constraint_triggers(&self) -> Arc<TriggerCatalog>;
}

impl ConstraintNameCatalog for CatalogReadSnapshot {
    fn constraint_table(&self, relation: &RelationIdentity) -> Option<ConstraintTableMetadata> {
        self.tables.get(relation).map(Into::into)
    }
    fn constraint_owner(
        &self,
        object_id: [u8; 16],
    ) -> Option<(RelationIdentity, ConstraintTableMetadata)> {
        self.tables
            .iter()
            .find(|(_, table)| table.object_id == object_id)
            .map(|(relation, table)| (relation.clone(), table.into()))
    }
    fn constraint_schema_tables(&self, schema: &str) -> Vec<ConstraintTableMetadata> {
        self.tables
            .iter()
            .filter(|(relation, _)| relation.schema == schema)
            .map(|(_, table)| table.into())
            .collect()
    }
    fn constraint_foreign_tables(&self) -> Arc<BTreeMap<RelationIdentity, StoredForeignTable>> {
        Arc::clone(&self.definitions.foreign_tables)
    }
    fn constraint_domains(&self) -> Arc<DomainRegistry> {
        Arc::clone(&self.definitions.domains)
    }
    fn constraint_triggers(&self) -> Arc<TriggerCatalog> {
        Arc::clone(&self.definitions.triggers)
    }
}
