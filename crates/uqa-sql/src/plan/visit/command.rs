//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow command expressions directly, including plans retained by utility commands.

use super::{try_visit_ctes, CommandPlan, QueryPlan, ScalarExpr};
fn subqueries<'a, E>(
    values: &'a [QueryPlan],
    visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), E>,
) -> Result<(), E> {
    for query in values {
        query.try_visit_scalar_expressions(visit)?;
    }
    Ok(())
}

impl CommandPlan {
    /// Visit scalar roots without copying the plan or collecting its children. Mutations retain their analysis order: CTEs, direct expressions, query inputs, then source inputs. Query-bearing utilities share the scalar rewrite's ownership boundary.
    pub fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        let Ok(()) = self.try_visit_scalar_expressions(&mut |expression| {
            visit(expression);
            Ok::<_, std::convert::Infallible>(())
        });
    }

    /// Stop at the first callback error, without copying syntax or visiting later inputs.
    pub fn try_visit_scalar_expressions<'a, E>(
        &'a self,
        visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Insert(_) | Self::Update(_) | Self::Delete(_) | Self::Merge(_) => {
                try_visit_ctes(self.ctes(), visit)?;
                for expression in self.expressions() {
                    visit(expression)?;
                }
                for query in self.query_inputs() {
                    query.try_visit_scalar_expressions(visit)?;
                }
                if let Some(source) = self.source_input() {
                    source.try_visit_scalar_expressions(visit)?;
                }
            }
            Self::CreateView { query, .. }
            | Self::CreateMaterializedView { query, .. }
            | Self::CreateTableAs { query, .. }
            | Self::DeclareCursor { query, .. } => query.try_visit_scalar_expressions(visit)?,
            Self::Explain { body, .. } | Self::Prepare { body, .. } => {
                body.try_visit_scalar_expressions(visit)?;
            }
            Self::Execute { params, .. } | Self::Call { args: params, .. } => {
                for expression in params {
                    visit(&expression.scalar)?;
                    subqueries(&expression.subqueries, visit)?;
                }
            }
            Self::CreateTable(_)
            | Self::CreateTableIfNotExists(_)
            | Self::CreateIndex(_)
            | Self::RenameIndex(_)
            | Self::Drop(_)
            | Self::AlterTable(_)
            | Self::AlterForeignTable(_)
            | Self::AlterView(_)
            | Self::RefreshMaterializedView { .. }
            | Self::CreateSchema { .. }
            | Self::AlterSchemaOwner { .. }
            | Self::RenameSchema { .. }
            | Self::Notify { .. }
            | Self::Listen { .. }
            | Self::Unlisten { .. }
            | Self::SetVariable { .. }
            | Self::ResetVariable { .. }
            | Self::ResetAllVariables
            | Self::SetConstraints { .. }
            | Self::ShowVariable { .. }
            | Self::Discard { .. }
            | Self::Load { .. }
            | Self::Analyze { .. }
            | Self::Vacuum(_)
            | Self::LockTable(_)
            | Self::Truncate { .. }
            | Self::Transaction(_)
            | Self::FetchCursor(_)
            | Self::CloseCursor { .. }
            | Self::CreateSequence(_)
            | Self::CreateDomain(_)
            | Self::AlterDomain(_)
            | Self::CommentType(_)
            | Self::CreateShellType(_)
            | Self::CreateEnum(_)
            | Self::CreateCompositeType(_)
            | Self::AlterEnum(_)
            | Self::AlterTypeObject(_)
            | Self::GrantType(_)
            | Self::AlterSequence(_)
            | Self::Deallocate { .. }
            | Self::CreateForeignWrapper(_)
            | Self::CreateForeignServer(_)
            | Self::CreateForeignTable(_)
            | Self::CreateForeignTableDefinition(_)
            | Self::CreateFunction(_)
            | Self::DropFunction(_)
            | Self::AlterRoutine(_)
            | Self::AlterRoutineOwner(_)
            | Self::RenameRoutine(_)
            | Self::GrantRoutine(_)
            | Self::GrantTable(_)
            | Self::GrantSequence(_)
            | Self::GrantDatabase(_)
            | Self::GrantSchema(_)
            | Self::GrantRole(_)
            | Self::CreateRole(_)
            | Self::AlterRole(_)
            | Self::RenameRole(_)
            | Self::DropRole(_)
            | Self::CreateTrigger(_)
            | Self::DropTrigger(_)
            | Self::CreateRule(_)
            | Self::DropRule(_)
            | Self::DoBlock { .. }
            | Self::Empty => {}
        }
        Ok(())
    }
}
