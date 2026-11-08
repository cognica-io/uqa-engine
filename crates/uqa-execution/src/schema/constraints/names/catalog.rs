//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current constraint declarations selected independently of a retained query catalog.

use crate::catalog::{domain::DomainRegistry, CatalogReadSnapshot, CatalogTableSnapshot};
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_sql::catalog::table_definition::SharedTableDefinition;
use uqa_sql::catalog::table_metadata::SharedTableMetadata;
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableHierarchy, TableKeyConstraint},
    catalog::events::TriggerCatalog,
    schema::constraint_changes::names::ConstraintNames,
};

pub struct ConstraintTableMetadata {
    pub object_id: [u8; 16],
    pub columns: SharedTableDefinition<Vec<ColumnDef>>,
    pub checks: SharedTableDefinition<Vec<TableCheck>>,
    pub foreign_keys: SharedTableMetadata<Vec<ForeignKey>>,
    pub keys: SharedTableMetadata<Vec<TableKeyConstraint>>,
    pub hierarchy: SharedTableDefinition<TableHierarchy>,
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
            columns: table.columns.clone(),
            checks: table.checks.clone(),
            foreign_keys: table.foreign_keys.clone(),
            keys: table.keys.clone(),
            hierarchy: table.hierarchy.clone(),
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
    fn constraint_foreign_tables(&self) -> Arc<crate::catalog::foreign::ForeignTables>;
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
    fn constraint_foreign_tables(&self) -> Arc<crate::catalog::foreign::ForeignTables> {
        Arc::clone(&self.definitions.foreign_tables)
    }
    fn constraint_domains(&self) -> Arc<DomainRegistry> {
        Arc::clone(&self.definitions.domains)
    }
    fn constraint_triggers(&self) -> Arc<TriggerCatalog> {
        Arc::clone(&self.definitions.triggers)
    }
}
