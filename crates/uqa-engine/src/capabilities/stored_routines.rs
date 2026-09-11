//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture catalog binding scopes for stored statements and scalar expressions.

use crate::Engine;
use uqa_sql::SQLError;

impl Engine {
    pub(crate) fn catalog_routine_analysis_context(
        &self,
    ) -> uqa_sql::binding::stored_routines::analysis::CatalogRoutineAnalysisContext<'_> {
        uqa_sql::binding::stored_routines::analysis::CatalogRoutineAnalysisContext {
            scopes: self,
            routines: self,
        }
    }
}
impl uqa_sql::binding::stored_routines::analysis::CatalogRoutineScopes for Engine {
    fn with_catalog_scope(
        &self,
        analyze: uqa_sql::binding::statements::StatementAnalysisOperation<'_>,
    ) -> Result<(), SQLError> {
        let scope = crate::capabilities::query_scope::new_for_catalog_binding(self);
        analyze(&scope)
    }
}
