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

pub use uqa_sql::catalog::security::SequenceSecurity;

pub use uqa_sql::catalog::security::TableSecurity;

pub use uqa_sql::catalog::security::SchemaSecurity;

pub mod role_lifecycle;
pub mod roles;

pub mod database_lifecycle;

pub mod sequence_lifecycle;

pub mod table_inquiry;

pub mod table_grants;
