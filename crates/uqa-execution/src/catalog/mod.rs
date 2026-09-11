//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable catalog inputs and runtime catalog projections.

pub mod foreign;
pub mod identity;
pub mod security;
pub mod sequence;
pub mod view;

use security::{DatabaseSecurity, SchemaSecurity, SequenceSecurity, TableSecurity};
use sequence::SequenceState;
use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::RelationIdentity;
pub use uqa_sql::catalog::resolution::{RelationLookupMode, RelationNameResolution};
use view::StoredView;
mod analysis;
pub mod graph;
mod read;
pub mod schema;

/// Read-only access to catalog-owned state. This view cannot mutate transactions, acquire locks, publish caches, or recover the enclosing engine.
#[derive(Clone)]
pub struct CatalogReadView {
    snapshot: Arc<CatalogReadSnapshot>,
}

/// Immutable catalog names and durable registries captured at one statement boundary.
#[derive(Clone)]
pub struct CatalogReadSnapshot {
    pub tables: BTreeMap<uqa_core::RelationIdentity, CatalogTableSnapshot>,
    pub definitions: CatalogDefinitionSnapshot,
}

/// Immutable table-definition fields used by binding and catalog projection.
#[derive(Clone)]
pub struct CatalogTableSnapshot {
    pub object_id: [u8; 16],
    pub security: Arc<crate::catalog::security::TableSecurity>,
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
    pub graphs: Arc<BTreeMap<String, Arc<uqa_graph::GraphStoreHandle>>>,
    pub views: Arc<BTreeMap<RelationIdentity, StoredView>>,
    pub catalog_indexes: Arc<BTreeMap<RelationIdentity, uqa_storage::CatalogIndexRow>>,
    pub database_security: Arc<DatabaseSecurity>,
    pub schemas: Arc<BTreeMap<String, SchemaSecurity>>,
    pub sequences: Arc<BTreeMap<RelationIdentity, SequenceState>>,
    pub sequence_object_ids: Arc<BTreeMap<RelationIdentity, [u8; 16]>>,
    pub sequence_security: Arc<BTreeMap<RelationIdentity, SequenceSecurity>>,
    pub foreign_table_security: Arc<BTreeMap<RelationIdentity, TableSecurity>>,
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
        }
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
