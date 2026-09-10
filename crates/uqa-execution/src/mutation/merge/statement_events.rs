//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement trigger events shared by table and view MERGE execution.

use crate::mutation::triggers::context::TriggerContext;
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

    pub fn fire_before(
        &self,
        context: &TriggerContext<'_>,
        relation: &str,
    ) -> Result<(), SQLError> {
        for (enabled, event, columns) in self.before_order() {
            if enabled {
                crate::mutation::triggers::fire_statement_triggers(
                    context,
                    relation,
                    uqa_sql::ast::TriggerTiming::Before,
                    event,
                    columns,
                )?;
            }
        }
        Ok(())
    }

    pub fn fire_after(&self, context: &TriggerContext<'_>, relation: &str) -> Result<(), SQLError> {
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
                crate::mutation::triggers::fire_statement_triggers(
                    context,
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

impl MergeStatementEvents {
    pub(super) fn fire_table_after(
        &self,
        context: &TriggerContext<'_>,
        target_table: &str,
        events: &crate::mutation::events::MutationEventQueue,
    ) -> Result<(), SQLError> {
        let delete_transition = if self.delete {
            crate::mutation::triggers::build_transition_tables(
                context,
                target_table,
                uqa_sql::ast::TriggerEvent::Delete,
                &[],
                events.after_rows(),
            )?
        } else {
            Vec::new()
        };
        let update_transition = if self.update {
            crate::mutation::triggers::build_transition_tables(
                context,
                target_table,
                uqa_sql::ast::TriggerEvent::Update,
                &self.updated_columns,
                events.after_rows(),
            )?
        } else {
            Vec::new()
        };
        let insert_transition = if self.insert {
            crate::mutation::triggers::build_transition_tables(
                context,
                target_table,
                uqa_sql::ast::TriggerEvent::Insert,
                &[],
                events.after_rows(),
            )?
        } else {
            Vec::new()
        };
        let referential_transition = events.referential_transition_tables(context)?;
        let mut transition_tables = delete_transition
            .iter()
            .chain(update_transition.iter())
            .chain(insert_transition.iter())
            .collect::<Vec<_>>();
        transition_tables.extend(referential_transition.iter());
        let root_events = [
            (self.delete, uqa_sql::ast::TriggerEvent::Delete),
            (self.update, uqa_sql::ast::TriggerEvent::Update),
            (self.insert, uqa_sql::ast::TriggerEvent::Insert),
        ]
        .into_iter()
        .filter_map(|(enabled, event)| enabled.then_some(event))
        .collect::<Vec<_>>();
        for generation in crate::mutation::triggers::after_trigger_generations(&transition_tables) {
            crate::mutation::triggers::fire_after_row_trigger_events_for_generation(
                context,
                events.after_rows(),
                &transition_tables,
                generation,
            )?;
            events.fire_referential_after_statement_triggers(
                context,
                &referential_transition,
                target_table,
                &root_events,
                generation,
            )?;
            for (enabled, event, columns) in [
                (self.delete, uqa_sql::ast::TriggerEvent::Delete, &[][..]),
                (
                    self.update,
                    uqa_sql::ast::TriggerEvent::Update,
                    self.updated_columns.as_slice(),
                ),
                (self.insert, uqa_sql::ast::TriggerEvent::Insert, &[][..]),
            ] {
                if !enabled {
                    continue;
                }
                let event_transitions = match event {
                    uqa_sql::ast::TriggerEvent::Delete => &delete_transition,
                    uqa_sql::ast::TriggerEvent::Update => &update_transition,
                    uqa_sql::ast::TriggerEvent::Insert => &insert_transition,
                    uqa_sql::ast::TriggerEvent::Truncate => unreachable!(),
                };
                crate::mutation::triggers::fire_after_statement_trigger_generation_for_root(
                    context,
                    target_table,
                    event,
                    columns,
                    event_transitions,
                    generation,
                )?;
            }
        }
        Ok(())
    }
}
