//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE traversal within stored statements, including nested command CTEs.

use super::{BTreeSet, MergeCallback, SQLError, Statement};

struct Selected<F>(F);
impl<W, F: FnMut(&mut crate::ast::MergeStmt) -> Result<(), SQLError>> super::Actions<W>
    for Selected<F>
{
    fn merge(&mut self, value: &mut crate::ast::MergeStmt) -> Result<(), SQLError> {
        (self.0)(value)
    }
}
pub fn visit_stored_statement_merges(
    statement: &mut Statement,
    visit: MergeCallback<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(visit_stored_statement_merges_from(
        statement,
        visit,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        &mut (),
    ))
}
pub(crate) async fn visit_stored_statement_merges_from<W: super::Work + Send>(
    statement: &mut Statement,
    visit: impl FnMut(&mut crate::ast::MergeStmt) -> Result<(), SQLError>,
    control: &uqa_core::memory::ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    super::Reader::new(&mut Selected(visit), control, work)
        .bind_statement(statement)
        .await
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
