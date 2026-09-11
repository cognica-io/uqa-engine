//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind named catalog definition operations to the owning Engine registries.

use uqa_sql::{ast::GrantTableStmt, SQLError};

use crate::Engine;
use uqa_execution::statement::context::definitions::TablePrivileges;

impl TablePrivileges for Engine {
    fn grant_table_privileges(&self, statement: &GrantTableStmt) -> Result<(), SQLError> {
        Engine::grant_table_privileges(self, statement)
    }
}
