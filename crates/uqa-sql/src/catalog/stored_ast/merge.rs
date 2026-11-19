//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE traversal within stored statements, including nested command CTEs.

use super::{BTreeSet, MergeCallback, SQLError, Statement, StoredAstVisitor};

pub fn visit_stored_statement_merges(
    statement: &mut Statement,
    visit: MergeCallback<'_>,
) -> Result<(), SQLError> {
    StoredAstVisitor {
        source: None,
        merge: Some(visit),
        expression: None,
        projection: None,
        ty: None,
        relation: &mut |_: &mut String| Ok(()),
        routine: &mut |_: &mut String, _: Option<&mut Option<crate::ast::FunctionBinding>>| Ok(()),
    }
    .bind_statement(statement)
}

use super::{Actions, Reader, Work};

impl<W: Work + Send, A: Actions<W>> Reader<'_, '_, A, W> {
    pub(super) async fn bind_merge(
        &mut self,
        merge: &mut crate::ast::MergeStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
        self.actions.merge(merge)?;
        self.actions.relation(&mut merge.target)?;
        let ctes = self.bind_ctes(&mut merge.with, inherited).await?;
        self.bind_from(&mut merge.source, &ctes).await?;
        self.bind_expr(&mut merge.join_condition, &ctes).await?;
        for clause in &mut merge.when_clauses {
            match clause {
                crate::ast::MergeWhen::UpdateMatched {
                    condition,
                    assignments,
                }
                | crate::ast::MergeWhen::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, &ctes).await?;
                    }
                    self.bind_assignments(assignments, &ctes).await?;
                }
                crate::ast::MergeWhen::InsertNotMatched {
                    condition,
                    columns,
                    values,
                    ..
                } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, &ctes).await?;
                    }
                    self.bind_targets(columns, &ctes).await?;
                    for expression in values {
                        self.bind_expr(expression, &ctes).await?;
                    }
                }
                crate::ast::MergeWhen::DeleteMatched { condition }
                | crate::ast::MergeWhen::DeleteNotMatchedBySource { condition }
                | crate::ast::MergeWhen::NothingMatched { condition }
                | crate::ast::MergeWhen::NothingNotMatched { condition }
                | crate::ast::MergeWhen::NothingNotMatchedBySource { condition } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, &ctes).await?;
                    }
                }
            }
        }
        for projection in &mut merge.returning {
            self.bind_projection(projection, &ctes).await?;
        }
        Ok(())
    }
}
