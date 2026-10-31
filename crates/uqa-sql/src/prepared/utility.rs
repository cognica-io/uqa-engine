//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned utility descriptors and the distinction between analyzed and unplanned statements.

use crate::{
    plan::{CommandPlan, PlanRef, UnifiedPlan},
    RowSchema, SQLError, Statement,
};

/// Read existing descriptors without executing a utility or advancing a portal.
pub trait PreparedSessionMetadata {
    fn show_result_schema(&self, name: &str) -> Result<RowSchema, SQLError>;
    fn portal_result_schema(&self, name: &str) -> Option<RowSchema>;
    fn prepared_result_schema(&self, name: &str) -> Option<RowSchema>;

    fn show_result_schema_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        control.check()?;
        self.show_result_schema(name)
    }
    fn portal_result_schema_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<RowSchema>, SQLError> {
        control.check()?;
        Ok(self.portal_result_schema(name))
    }
    fn prepared_result_schema_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<RowSchema>, SQLError> {
        control.check()?;
        Ok(self.prepared_result_schema(name))
    }
}

/// Utilities whose bodies undergo parse analysis also require a planning snapshot and cache revalidation. Other utilities retain their original result descriptor even when the session object used at execution changes.
pub fn requires_analysis(plan: &UnifiedPlan) -> bool {
    requires_analysis_ref(plan.into())
}

pub fn requires_analysis_ref(plan: PlanRef<'_>) -> bool {
    match plan {
        PlanRef::Query(_) => true,
        PlanRef::Command(command) => matches!(
            command,
            CommandPlan::Insert(_)
                | CommandPlan::Update(_)
                | CommandPlan::Delete(_)
                | CommandPlan::Merge(_)
                | CommandPlan::Call { .. }
                | CommandPlan::Explain { .. }
                | CommandPlan::DeclareCursor { .. }
                | CommandPlan::CreateTableAs { .. }
                | CommandPlan::CreateMaterializedView { .. }
        ),
    }
}

pub fn statement_requires_analysis(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Select(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Merge(_)
            | Statement::Call { .. }
            | Statement::Explain { .. }
            | Statement::DeclareCursor(_)
            | Statement::CreateTableAs { .. }
            | Statement::CreateMaterializedView { .. }
    )
}

pub fn result_schema(
    session: &dyn PreparedSessionMetadata,
    plan: &UnifiedPlan,
) -> Result<Option<RowSchema>, SQLError> {
    let UnifiedPlan::Command(command) = plan else {
        return Ok(None);
    };
    match command.as_ref() {
        CommandPlan::ShowVariable { name } => session.show_result_schema(name).map(Some),
        CommandPlan::FetchCursor(fetch) if !fetch.move_only => {
            Ok(session.portal_result_schema(&fetch.name))
        }
        CommandPlan::Execute { name, .. } => Ok(session.prepared_result_schema(name)),
        _ => Ok(None),
    }
}

/// Observe utility metadata under the same allowance as its preparation; shared portal and prepared schemas retain their original owners.
pub fn result_schema_with_control(
    session: &dyn PreparedSessionMetadata,
    plan: PlanRef<'_>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<RowSchema>, SQLError> {
    control.check()?;
    let PlanRef::Command(command) = plan else {
        return Ok(None);
    };
    match command {
        CommandPlan::ShowVariable { name } => session
            .show_result_schema_with_control(name, control)
            .map(Some),
        CommandPlan::FetchCursor(fetch) if !fetch.move_only => {
            session.portal_result_schema_with_control(&fetch.name, control)
        }
        CommandPlan::Execute { name, .. } => {
            session.prepared_result_schema_with_control(name, control)
        }
        _ => Ok(None),
    }
}
