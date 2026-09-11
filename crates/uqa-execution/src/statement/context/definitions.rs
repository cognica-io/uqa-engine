//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table privilege catalog entry points.

use uqa_sql::{ast::GrantTableStmt, SQLError};

pub trait TablePrivileges {
    fn grant_table_privileges(&self, statement: &GrantTableStmt) -> Result<(), SQLError>;
}
