//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authorization values and ACL operations for catalog execution.

pub mod columns;
pub mod schema;
pub mod sequence;
pub mod table;

pub use uqa_sql::catalog::security::database::{
    BoundDatabaseSecurity, DatabaseAclEntry, DatabasePrivileges, DatabaseSecurity,
};

pub use uqa_sql::catalog::security::SequenceSecurity;

pub use uqa_sql::catalog::security::{BoundTableSecurity, TableSecurity};

pub use uqa_sql::catalog::security::{BoundSchemaSecurity, SchemaSecurity};

pub mod role_lifecycle;
pub mod roles;

pub mod database_lifecycle;

pub mod routine_inquiry;
pub mod sequence_lifecycle;

pub mod table_inquiry;

pub mod system_relations;
pub mod table_grants;

pub mod foreign_authorization;
pub mod table_authorization;
pub mod table_maintenance;

pub mod table_ownership;

pub mod relation_restoration;
