//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement trigger events shared by table and view MERGE execution.

use crate::mutation::triggers::{context::TriggerContext, queue::StatementEvent};
use std::collections::BTreeSet;
use uqa_sql::{
    plan::{MergePlan, MergeWhenPlan},
    SQLError,
};

pub struct MergeStatementEvents {
    pub insert: bool,
    pub update: bool,
    pub delete: bool,
    pub updated_columns: Vec<String>,
}

impl MergeStatementEvents {
    pub fn from_plan(plan: &MergePlan) -> Self {
        let insert = plan
            .when_clauses
            .iter()
            .any(|clause| matches!(clause, MergeWhenPlan::InsertNotMatched { .. }));
        let update = plan.when_clauses.iter().any(|clause| {
            matches!(
                clause,
                MergeWhenPlan::UpdateMatched { .. }
                    | MergeWhenPlan::UpdateNotMatchedBySource { .. }
            )
        });
        let delete = plan.when_clauses.iter().any(|clause| {
            matches!(
                clause,
                MergeWhenPlan::DeleteMatched { .. }
                    | MergeWhenPlan::DeleteNotMatchedBySource { .. }
            )
        });
        let updated_columns = plan
            .when_clauses
            .iter()
            .filter_map(|clause| match clause {
                MergeWhenPlan::UpdateMatched { assignments, .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => Some(assignments),
                _ => None,
            })
            .flatten()
            .map(|assignment| assignment.target.column.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            insert,
            update,
            delete,
            updated_columns,
        }
    }

    pub fn has_before_statement_trigger(
        &self,
        context: &TriggerContext<'_>,
        relation: &str,
    ) -> Result<bool, SQLError> {
        for (enabled, event, columns) in self.before_order() {
            if enabled
                && !context
                    .catalog
                    .triggers_for(
                        relation,
                        uqa_sql::ast::TriggerTiming::Before,
                        event,
                        false,
                        columns,
                    )?
                    .is_empty()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The relations and operations whose BEFORE STATEMENT triggers the MERGE fires, in their order: INSERT, UPDATE, then DELETE, for each action kind it holds.
    pub fn before_statements(&self, relation: &str) -> Vec<StatementEvent> {
        self.before_order()
            .into_iter()
            .filter(|(enabled, _, _)| *enabled)
            .map(|(_, event, columns)| StatementEvent::new(relation, event, columns))
            .collect()
    }

    /// The relations and operations whose AFTER STATEMENT triggers the MERGE fires, in their order: DELETE, UPDATE, then INSERT.
    pub fn after_statements(&self, relation: &str) -> Vec<StatementEvent> {
        [
            (self.delete, uqa_sql::ast::TriggerEvent::Delete, &[][..]),
            (
                self.update,
                uqa_sql::ast::TriggerEvent::Update,
                self.updated_columns.as_slice(),
            ),
            (self.insert, uqa_sql::ast::TriggerEvent::Insert, &[][..]),
        ]
        .into_iter()
        .filter(|(enabled, _, _)| *enabled)
        .map(|(_, event, columns)| StatementEvent::new(relation, event, columns))
        .collect()
    }

    fn before_order(&self) -> [(bool, uqa_sql::ast::TriggerEvent, &[String]); 3] {
        [
            (self.insert, uqa_sql::ast::TriggerEvent::Insert, &[][..]),
            (
                self.update,
                uqa_sql::ast::TriggerEvent::Update,
                self.updated_columns.as_slice(),
            ),
            (self.delete, uqa_sql::ast::TriggerEvent::Delete, &[][..]),
        ]
    }
}
