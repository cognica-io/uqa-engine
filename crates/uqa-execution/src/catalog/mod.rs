//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable catalog inputs and runtime catalog projections.

pub mod composite_type;
pub mod domain;
pub mod enum_type;
pub mod foreign;
pub mod identity;
pub mod security;
pub mod sequence;
pub mod type_identity_restoration;
pub(crate) mod type_records;
pub mod value_restoration;
pub mod view;

use security::{
    BoundDatabaseSecurity, BoundSchemaSecurity, BoundSequenceSecurity, BoundTableSecurity,
};
use sequence::SequenceState;
use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::RelationIdentity;
pub use uqa_sql::catalog::resolution::{RelationLookupMode, RelationNameResolution};
use view::StoredView;
mod analysis;
pub mod graph;
pub mod graph_oids;
mod graph_reads;
pub mod namespaces;
mod prepared_dependencies;
mod read;
pub mod schema;
mod snapshot_read;

/// Immutable catalog inputs with optional query read attribution. This view cannot change definitions, coordinate DDL, publish caches, or recover the enclosing engine.
#[derive(Clone)]
pub struct CatalogReadView {
    snapshot: Arc<CatalogReadSnapshot>,
    graph_reads: Option<Arc<graph_reads::GraphCatalogRead>>,
    /// The owning session's catalog records, read only inside its retained statement transaction.
    prepared_catalog: Option<Arc<dyn uqa_storage::CatalogFacade>>,
    /// The exact positions of sequences whose durable records run ahead of the values handed out. They move without a catalog change, so they are read when a sequence's values are.
    sequence_positions: Option<Arc<crate::row_locks::RowLockManager>>,
    /// The committed value records of sequences, which also move without a catalog change. A sequence's state is read from them when no position covers it.
    latest_sequence_values: Option<Arc<dyn sequence::latest_values::LatestSequenceValues>>,
}

/// Immutable catalog names and durable registries captured at one statement boundary.
#[derive(Clone)]
pub struct CatalogReadSnapshot {
    pub tables: BTreeMap<uqa_core::RelationIdentity, CatalogTableSnapshot>,
    pub definitions: CatalogDefinitionSnapshot,
    /// The session's temporary namespace once its first temporary object created it.
    pub temporary_namespace: Option<uqa_sql::catalog::temporary_namespace::TemporaryNamespace>,
}

/// Immutable table-definition fields used by binding and catalog projection.
#[derive(Clone)]
pub struct CatalogTableSnapshot {
    pub object_id: [u8; 16],
    /// The table's public OIDs, recorded or derived from its identity.
    pub catalog_oids: uqa_sql::catalog::relation_oids::RelationCatalogOids,
    pub security: Arc<crate::catalog::security::BoundTableSecurity>,
    pub columns: Arc<Vec<uqa_sql::ast::ColumnDef>>,
    pub columns_declared: bool,
    pub checks: Arc<Vec<uqa_sql::ast::TableCheck>>,
    pub foreign_keys: Arc<Vec<uqa_sql::ast::ForeignKey>>,
    pub keys: Arc<Vec<uqa_sql::ast::TableKeyConstraint>>,
    pub hierarchy: Arc<uqa_sql::ast::TableHierarchy>,
    pub persistence: uqa_sql::ast::RelationPersistence,
}

/// Immutable state for one sequence selected through the statement's relation namespace.
#[derive(Clone)]
pub struct CatalogSequenceSnapshot {
    pub relation: uqa_core::RelationIdentity,
    pub state: crate::catalog::sequence::SequenceState,
    pub security: crate::catalog::security::SequenceSecurity,
}

pub type CatalogSequenceMetadata = (
    String,
    uqa_sql::ast::RelationPersistence,
    [u8; 16],
    security::SequenceSecurity,
);

pub use uqa_sql::catalog::resolution::RelationResolution;

/// Shared immutable definition maps captured from one catalog generation.
#[derive(Clone)]
pub struct CatalogDefinitionSnapshot {
    pub sequence_persistence: Arc<BTreeMap<RelationIdentity, uqa_sql::ast::RelationPersistence>>,
    pub foreign_tables: Arc<BTreeMap<RelationIdentity, foreign::StoredForeignTable>>,
    pub sql_user_functions: Arc<BTreeMap<String, Vec<Arc<uqa_sql::routines::SQLUserFunction>>>>,
    pub role_memberships: Arc<
        BTreeMap<
            uqa_sql::catalog::roles::RoleMembershipKey,
            uqa_sql::catalog::roles::RoleMembership,
        >,
    >,

    pub domains: Arc<BTreeMap<String, uqa_sql::catalog::domain::StoredDomain>>,
    pub enums: Arc<enum_type::EnumRegistry>,
    pub composites: Arc<composite_type::CompositeRegistry>,
    pub graphs: Arc<BTreeMap<String, Arc<uqa_graph::GraphStoreHandle>>>,
    pub views: Arc<BTreeMap<RelationIdentity, StoredView>>,
    pub catalog_indexes: Arc<BTreeMap<RelationIdentity, uqa_storage::CatalogIndexRow>>,
    pub database_security: Arc<BoundDatabaseSecurity>,
    pub schemas: Arc<BTreeMap<String, BoundSchemaSecurity>>,
    pub sequences: Arc<BTreeMap<RelationIdentity, SequenceState>>,
    pub sequence_object_ids: Arc<BTreeMap<RelationIdentity, [u8; 16]>>,
    /// The `pg_class` OIDs sequences recorded when they were created, by object identity.
    pub sequence_catalog_oids: Arc<BTreeMap<[u8; 16], u32>>,
    /// The OIDs graphs and their labels recorded when they were created, by graph name.
    pub graph_catalog_oids: Arc<BTreeMap<String, uqa_sql::catalog::graph_oids::GraphCatalogOids>>,
    pub sequence_security: Arc<BTreeMap<RelationIdentity, BoundSequenceSecurity>>,
    pub foreign_table_security: Arc<BTreeMap<RelationIdentity, BoundTableSecurity>>,
    pub system_relation_security:
        Arc<uqa_sql::catalog::security::system_relations::SystemRelationSecurities>,
    pub roles: Arc<BTreeMap<String, uqa_sql::catalog::roles::RoleDefinition>>,
    pub triggers: Arc<
        BTreeMap<
            uqa_storage::RelationIdentity,
            BTreeMap<String, uqa_sql::catalog::events::StoredTrigger>,
        >,
    >,
    pub rules: Arc<
        BTreeMap<
            uqa_storage::RelationIdentity,
            BTreeMap<String, uqa_sql::catalog::events::StoredRule>,
        >,
    >,
}

impl CatalogReadView {
    pub fn new(snapshot: CatalogReadSnapshot) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            graph_reads: None,
            prepared_catalog: None,
            sequence_positions: None,
            latest_sequence_values: None,
        }
    }

    #[must_use]
    pub fn with_sequence_positions(
        mut self,
        positions: Arc<crate::row_locks::RowLockManager>,
    ) -> Self {
        self.sequence_positions = Some(positions);
        self
    }

    #[must_use]
    pub fn with_latest_sequence_values(
        mut self,
        values: Arc<dyn sequence::latest_values::LatestSequenceValues>,
    ) -> Self {
        self.latest_sequence_values = Some(values);
        self
    }
    #[must_use]
    pub fn with_prepared_catalog(mut self, catalog: Arc<dyn uqa_storage::CatalogFacade>) -> Self {
        self.prepared_catalog = Some(catalog);
        self
    }

    pub fn snapshot(&self) -> &CatalogReadSnapshot {
        &self.snapshot
    }
}

pub mod cache;
pub mod context;
pub mod index;
pub mod projection;
pub mod services;

pub mod sequence_introspection;

#[cfg(test)]
pub(crate) mod test_support;

pub mod notices;
