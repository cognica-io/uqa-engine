//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture engine-owned session and routine state for a query execution scope.

pub(crate) type CteScope = uqa_execution::query::CteScope<crate::session::StatementReadSnapshot>;
use crate::Engine;
use uqa_core::memory::ProductionControl;
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{catalog::resolution::RelationLookupMode, SQLError};

pub(crate) fn new_for_current_routine(engine: &Engine) -> CteScope {
    new_for_current_routine_with_control(
        engine,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary current-routine scope capture")
}

pub(crate) fn new_for_statement(
    engine: &Engine,
    privilege_subject: Option<&RoleReference>,
) -> CteScope {
    let revision = engine.runtime.regtype_output_cache.revision();
    let mut scope = CteScope::with_catalog(
        engine.catalog_read_view(),
        engine.session_execution_view().relation_name_resolution(),
        privilege_subject.cloned(),
    );
    scope.set_catalog_revision(engine.session_id, revision);
    scope.rows.extend(
        uqa_execution::mutation::triggers::current_transition_relations()
            .into_iter()
            .map(|(name, rows)| {
                (
                    name,
                    uqa_execution::query::cte::rows::CteRows::Complete(rows),
                )
            }),
    );
    if crate::roles::active_routine_reads_command_overlay() == Some(false) {
        scope.set_reads_command_overlay(false);
    }
    scope
}

pub(crate) fn new_for_command(
    engine: &Engine,
    privilege_subject: Option<&RoleReference>,
    relations_bound: bool,
) -> Result<CteScope, SQLError> {
    let mut scope = new_for_statement(engine, privilege_subject);
    if relations_bound {
        scope.set_relation_lookup_mode(RelationLookupMode::Bound)?;
    }
    Ok(scope)
}

pub(crate) fn new_for_catalog_binding(engine: &Engine) -> CteScope {
    let revision = engine.runtime.regtype_output_cache.revision();
    let mut resolution = engine.session_execution_view().relation_name_resolution();
    resolution.set_lookup_mode(RelationLookupMode::Bound);
    let mut scope = CteScope::with_catalog(engine.restored_catalog_read_view(), resolution, None);
    scope.set_catalog_revision(engine.session_id, revision);
    scope
}

impl
    uqa_execution::mutation::command_scope::CommandScopeSource<
        crate::session::StatementReadSnapshot,
    > for Engine
{
    fn read_control(&self) -> Result<uqa_storage::read_control::StorageReadControl, SQLError> {
        self.query_retention_control()
    }
    fn cancellation(&self) -> &uqa_core::CancellationToken {
        self.query_runtime_view().cancellation
    }
    fn command_scope(
        &self,
        privilege_subject: Option<&RoleReference>,
        relations_bound: bool,
    ) -> Result<CteScope, SQLError> {
        new_for_command(self, privilege_subject, relations_bound)
    }
}

impl uqa_sql::binding::statements::StatementAnalysisScopes for Engine {
    fn planning_control(&self) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
        let resources = self.query_retention_control()?;
        Ok(ProductionControl::new(
            resources.memory(),
            resources.cancellation(),
            &self.runtime.cancellation,
        )
        .capture())
    }

    fn with_controlled_scope(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
        analyze: uqa_sql::binding::statements::ControlledStatementAnalysisOperation<'_>,
    ) -> Result<(), SQLError> {
        let scope = new_for_current_routine_with_control(self, control)?;
        analyze(uqa_execution::query::binding::binding_context_with_control(
            &scope, control,
        )?)
    }

    fn with_scope(
        &self,
        analyze: uqa_sql::binding::statements::StatementAnalysisOperation<'_>,
    ) -> Result<(), SQLError> {
        let scope = new_for_current_routine(self);
        analyze(&scope)
    }
}

pub(crate) fn new_for_current_routine_with_control(
    engine: &Engine,
    control: &ProductionControl<'_>,
) -> Result<CteScope, SQLError> {
    control.check()?;
    let revision = engine.runtime.regtype_output_cache.revision();
    let resolution =
        uqa_execution::catalog::services::CatalogSession::relation_name_resolution_with_control(
            &engine.session_execution_view(),
            control,
        )?;
    let mut scope = CteScope::with_catalog_with_control(
        engine.catalog_read_view_with_control(control)?,
        resolution,
        control,
    )?;
    scope.set_catalog_revision(engine.session_id, revision);
    uqa_execution::mutation::triggers::with_current_transition_relations(|relations| {
        if let Some(relations) = relations {
            for (name, rows) in relations {
                scope.insert_shared_with_control(name, rows.clone(), control)?;
            }
        }
        Ok::<_, SQLError>(())
    })?;
    if crate::roles::active_routine_reads_command_overlay() == Some(false) {
        scope.set_reads_command_overlay(false);
    }
    Ok(scope)
}

/// Catalog binding captures the restored immutable view and bound relation identities without inheriting the current statement's CTE namespace.
pub(crate) fn new_for_catalog_binding_with_control(
    engine: &Engine,
    control: &ProductionControl<'_>,
) -> Result<CteScope, SQLError> {
    control.check()?;
    let resolution =
        uqa_execution::catalog::services::CatalogSession::relation_name_resolution_with_control(
            &engine.session_execution_view(),
            control,
        )?;
    let (mut resolution, memory) = resolution.into_parts();
    resolution.set_lookup_mode(uqa_sql::catalog::resolution::RelationLookupMode::Bound);
    let mut scope = CteScope::with_catalog_with_control(
        engine.restored_catalog_read_view_with_control(control)?,
        control.finish(resolution, memory)?,
        control,
    )?;
    scope.set_catalog_revision(
        engine.session_id,
        engine.runtime.regtype_output_cache.revision(),
    );
    Ok(scope)
}
