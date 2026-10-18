//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedural traceback names preserve the primary failure even when catalog lookup fails.

use super::{ColumnType, Interpreter};

impl Interpreter<'_> {
    pub(super) fn statement_error_context(
        &self,
        line: i64,
        statement: &super::PLpgSQLStmt,
    ) -> String {
        let signature = self
            .def
            .catalog_oid
            .and_then(|oid| {
                self.services
                    .expressions
                    .resolve_regtype_output(&ColumnType::Regprocedure, i64::from(oid))
                    .ok()
                    .flatten()
            })
            .unwrap_or_else(|| {
                uqa_sql::routines::lifecycle::routine_signature_label(
                    &self.def.name,
                    &uqa_sql::routines::routine_signature_types(self.def),
                )
            });
        format!(
            "PL/pgSQL function {signature} line {line} at {}",
            statement.diagnostic_kind()
        )
    }
}
