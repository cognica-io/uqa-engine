//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE traversal within stored statements, including nested command CTEs.

use super::{BTreeSet, MergeCallback, SQLError, Statement, StoredAstVisitor};

pub(crate) fn visit_stored_statement_merges(
    statement: &mut Statement,
    visit: MergeCallback<'_>,
) -> Result<(), SQLError> {
    StoredAstVisitor {
        source: None,
        merge: Some(visit),
        expression: None,
        ty: None,
        relation: &mut |_: &mut String| Ok(()),
        routine: &mut |_: &mut String, _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>| Ok(()),
    }
    .bind_statement(statement)
}

impl<R, F> StoredAstVisitor<'_, R, F>
where
    R: FnMut(&mut String) -> Result<(), SQLError>,
    F: FnMut(
        &mut String,
        Option<&mut Option<uqa_sql::ast::FunctionBinding>>,
    ) -> Result<(), SQLError>,
{
    pub(super) fn bind_merge(
        &mut self,
        merge: &mut uqa_sql::ast::MergeStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        if let Some(visit) = &mut self.merge {
            visit(merge)?;
        }
        (self.relation)(&mut merge.target)?;
        let ctes = self.bind_ctes(&mut merge.with, inherited)?;
        self.bind_from(&mut merge.source, &ctes)?;
        self.bind_expr(&mut merge.join_condition, &ctes)?;
        for clause in &mut merge.when_clauses {
            match clause {
                uqa_sql::ast::MergeWhen::UpdateMatched {
                    condition,
                    assignments,
                }
                | uqa_sql::ast::MergeWhen::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, &ctes)?;
                    }
                    for (_, expression) in assignments {
                        self.bind_expr(expression, &ctes)?;
                    }
                }
                uqa_sql::ast::MergeWhen::InsertNotMatched {
                    condition, values, ..
                } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, &ctes)?;
                    }
                    for expression in values {
                        self.bind_expr(expression, &ctes)?;
                    }
                }
                uqa_sql::ast::MergeWhen::DeleteMatched { condition }
                | uqa_sql::ast::MergeWhen::DeleteNotMatchedBySource { condition }
                | uqa_sql::ast::MergeWhen::NothingMatched { condition }
                | uqa_sql::ast::MergeWhen::NothingNotMatched { condition }
                | uqa_sql::ast::MergeWhen::NothingNotMatchedBySource { condition } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, &ctes)?;
                    }
                }
            }
        }
        for projection in &mut merge.returning {
            self.bind_expr(&mut projection.expr, &ctes)?;
        }
        Ok(())
    }
}
