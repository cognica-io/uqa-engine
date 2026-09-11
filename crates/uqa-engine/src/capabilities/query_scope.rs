//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture engine-owned session and routine state for a query execution scope.

pub(crate) type CteScope = uqa_execution::query::CteScope<crate::session::StatementReadSnapshot>;
use crate::Engine;
use uqa_sql::{catalog::resolution::RelationLookupMode, SQLError};

pub(crate) fn new_for_current_routine(engine: &Engine) -> CteScope {
    new_for_statement(engine, None)
}

pub(crate) fn new_for_statement(engine: &Engine, privilege_subject: Option<&str>) -> CteScope {
    let mut scope = CteScope::with_catalog(
        engine.catalog_read_view(),
        engine.session_execution_view().relation_name_resolution(),
        privilege_subject.map(str::to_string),
    );
    scope
        .rows
        .extend(uqa_execution::mutation::triggers::current_transition_relations());
    if crate::roles::active_routine_reads_command_overlay() == Some(false) {
        scope.set_reads_command_overlay(false);
    }
    scope
}

pub(crate) fn new_for_command(
    engine: &Engine,
    privilege_subject: Option<&str>,
    relations_bound: bool,
) -> Result<CteScope, SQLError> {
    let mut scope = new_for_statement(engine, privilege_subject);
    if relations_bound {
        scope.set_relation_lookup_mode(RelationLookupMode::Bound)?;
    }
    Ok(scope)
}

pub(crate) fn new_for_catalog_binding(engine: &Engine) -> CteScope {
    let mut resolution = engine.session_execution_view().relation_name_resolution();
    resolution.set_lookup_mode(RelationLookupMode::Bound);
    CteScope::with_catalog(engine.restored_catalog_read_view(), resolution, None)
}

impl
    uqa_execution::mutation::command_scope::CommandScopeSource<
        crate::session::StatementReadSnapshot,
    > for Engine
{
    fn command_scope(
        &self,
        privilege_subject: Option<&str>,
        relations_bound: bool,
    ) -> Result<CteScope, SQLError> {
        new_for_command(self, privilege_subject, relations_bound)
    }
}

impl uqa_sql::binding::statements::StatementAnalysisScopes for Engine {
    fn with_scope(
        &self,
        analyze: uqa_sql::binding::statements::StatementAnalysisOperation<'_>,
    ) -> Result<(), SQLError> {
        let scope = new_for_current_routine(self);
        analyze(&scope)
    }
}
