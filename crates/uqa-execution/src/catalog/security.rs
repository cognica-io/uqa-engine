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
    DatabaseAclEntry, DatabasePrivileges, DatabaseSecurity,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceSecurity {
    pub role_owner: String,
    pub acl: Option<Vec<uqa_storage::SequenceAclEntry>>,
}

pub use uqa_sql::catalog::security::TableSecurity;

pub use uqa_sql::catalog::security::SchemaSecurity;

pub mod roles;

pub mod database_lifecycle;
