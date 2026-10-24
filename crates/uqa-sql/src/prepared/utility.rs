//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned utility descriptors and the distinction between analyzed and unplanned statements.

use crate::{
    plan::{CommandPlan, UnifiedPlan},
    RowSchema, SQLError, Statement,
};

/// Read existing descriptors without executing a utility or advancing a portal.
pub trait PreparedSessionMetadata {
    fn show_result_schema(&self, name: &str) -> Result<RowSchema, SQLError>;
    fn portal_result_schema(&self, name: &str) -> Option<RowSchema>;
    fn prepared_result_schema(&self, name: &str) -> Option<RowSchema>;
}

/// Utilities whose bodies undergo parse analysis also require a planning snapshot and cache revalidation. Other utilities retain their original result descriptor even when the session object used at execution changes.
pub fn requires_analysis(plan: &UnifiedPlan) -> bool {
    match plan {
        UnifiedPlan::Query(_) => true,
        UnifiedPlan::Command(command) => matches!(
            command.as_ref(),
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
