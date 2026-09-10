//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement trigger events shared by table and view MERGE execution.

use super::{BTreeSet, Engine, MergePlan, MergeWhenPlan, SQLError};

pub(in crate::sql::dml) struct MergeStatementEvents {
    pub(in crate::sql::dml) insert: bool,
    pub(in crate::sql::dml) update: bool,
    pub(in crate::sql::dml) delete: bool,
    pub(in crate::sql::dml) updated_columns: Vec<String>,
}

impl MergeStatementEvents {
    pub(in crate::sql::dml) fn from_plan(plan: &MergePlan) -> Self {
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
            .map(|assignment| assignment.column.clone())
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

    pub(in crate::sql::dml) fn has_before_statement_trigger(
        &self,
        engine: &Engine,
        relation: &str,
    ) -> Result<bool, SQLError> {
        for (enabled, event, columns) in self.before_order() {
            if enabled
                && !engine
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

    pub(in crate::sql::dml) fn fire_before(
        &self,
        engine: &Engine,
        relation: &str,
    ) -> Result<(), SQLError> {
        for (enabled, event, columns) in self.before_order() {
            if enabled {
                crate::sql::triggers::fire_statement_triggers(
                    engine,
                    relation,
                    uqa_sql::ast::TriggerTiming::Before,
                    event,
                    columns,
                )?;
            }
        }
        Ok(())
    }

    pub(in crate::sql::dml) fn fire_after(
        &self,
        engine: &Engine,
        relation: &str,
    ) -> Result<(), SQLError> {
        for (enabled, event, columns) in [
            (self.delete, uqa_sql::ast::TriggerEvent::Delete, &[][..]),
            (
                self.update,
                uqa_sql::ast::TriggerEvent::Update,
                self.updated_columns.as_slice(),
            ),
            (self.insert, uqa_sql::ast::TriggerEvent::Insert, &[][..]),
        ] {
            if enabled {
                crate::sql::triggers::fire_statement_triggers(
                    engine,
                    relation,
                    uqa_sql::ast::TriggerTiming::After,
                    event,
                    columns,
                )?;
            }
        }
        Ok(())
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
