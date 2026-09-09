//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column dependencies of stored MERGE actions, including target assignments.

use uqa_sql::ast::{MergeStmt, MergeWhen, RuleEvent};

use super::{
    action_returning_scope, ColumnBindingContext, ColumnScope, RuleColumnBinder, SQLError,
};

impl RuleColumnBinder<'_> {
    pub(super) fn bind_merge(
        &mut self,
        merge: &mut MergeStmt,
        outer: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        let context = self.bind_ctes(&mut merge.with, outer, context)?;
        let target = self.table_scope(
            &merge.target,
            &merge.target_qualifier,
            merge.target_alias.as_deref(),
            &[],
            &ColumnBindingContext::default(),
        )?;
        let (local, scopes) =
            self.bind_dml_source(Some(&mut merge.source), &target, outer, &context)?;
        self.bind_expr(&mut merge.join_condition, &scopes, &context)?;
        for action in &mut merge.when_clauses {
            let condition = match action {
                MergeWhen::UpdateMatched {
                    condition,
                    assignments,
                }
                | MergeWhen::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => {
                    for (column, expression) in assignments {
                        self.bind_target_name(column, &target);
                        self.bind_expr(expression, &scopes, &context)?;
                    }
                    condition
                }
                MergeWhen::InsertNotMatched {
                    condition,
                    columns,
                    values,
                } => {
                    if columns.is_empty() {
                        *columns = target
                            .output
                            .iter()
                            .take(values.len())
                            .map(|column| column.current_name.clone())
                            .collect();
                    }
                    self.bind_target_names(columns, &target);
                    for expression in values {
                        self.bind_expr(expression, &scopes, &context)?;
                    }
                    condition
                }
                MergeWhen::DeleteMatched { condition }
                | MergeWhen::DeleteNotMatchedBySource { condition }
                | MergeWhen::NothingMatched { condition }
                | MergeWhen::NothingNotMatched { condition }
                | MergeWhen::NothingNotMatchedBySource { condition } => condition,
            };
            if let Some(condition) = condition {
                self.bind_expr(condition, &scopes, &context)?;
            }
        }
        let returning =
            action_returning_scope(&local, &target, RuleEvent::Update, &merge.returning_aliases);
        let mut scopes = vec![returning.clone()];
        scopes.extend_from_slice(outer);
        self.bind_projections(&mut merge.returning, Some(&returning), &scopes, &context)
    }
}
