//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict resolution, identity extraction, and RETURNING assembly.

pub(in crate::sql) use uqa_sql::semantics::returning::validate_returning_alias_relations;

mod returning;
pub(in crate::sql) use returning::{dml_command_returning_schema, dml_statement_returning_schema};
