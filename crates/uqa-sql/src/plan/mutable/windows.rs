//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical window refresh retains the existing query-local order across ready and scheduled child work.

use super::{
    edits::BodySlot, CommandQuerySlot, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
    MutableSourcePlan, QueryChildSlot, QueryScalarSlot, SourceChildSlot, SourceScalarSlot,
};
use crate::{
    catalog::work::Work as CatalogWork,
    plan::{
        retirement::construction::Prefix, windows::ResolvedWindows, CommandPlan, RelationalPlan,
        SourcePlan,
    },
    SQLError,
};
use input::Input;
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

mod input;
mod roots;

type Windows = Arc<Produced<ResolvedWindows>>;
#[derive(Clone)]
enum SourceScope {
    Queries,
    Roots(Windows),
}

impl MutableExecutablePlan {
    pub fn normalize_window_definitions(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(match self {
            Self::Query(value) => Self::Query(value.normalize_window_definitions(control)?),
            Self::Command(value) => Self::Command(value.normalize_window_definitions(control)?),
        })
    }
}
impl MutableQueryPlan {
    pub fn normalize_window_definitions(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        crate::catalog::work::inline(query(&mut self, control, &mut input::Immediate::new(None)))?;
        Ok(self)
    }
}
impl MutableCommandPlan {
    pub fn normalize_window_definitions(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        crate::catalog::work::inline(command(
            &mut self,
            control,
            &mut input::Immediate::new(None),
        ))?;
        Ok(self)
    }
}

impl Prefix<MutableQueryPlan> {
    pub(in crate::plan) fn normalize_window_definitions(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut input = input::Immediate::new(Some(self.context().clone()));
        crate::catalog::work::inline(query(&mut self, control, &mut input))?;
        control.check()?;
        Ok(self)
    }
}

async fn checkpoint<I: Input>(
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    control.check()?;
    CatalogWork::checkpoint(input.work()).await?;
    control.check()?;
    Ok(())
}

async fn body<I: Input>(
    slot: BodySlot<'_>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    match slot {
        BodySlot::Query(mut slot) => {
            let value = input.query(slot.take(), control)?.await?;
            slot.restore(value);
        }
        BodySlot::Command(mut slot) => {
            let value = input.command(slot.take(), control)?.await?;
            slot.restore(value);
        }
    }
    Ok(())
}

async fn query<I: Input>(
    query: &mut MutableQueryPlan,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    query.assert_owner(control);
    checkpoint(control, input).await?;
    for index in 0..query.ctes.len() {
        body(query.cte_slot(index), control, input).await?;
    }
    let subqueries = match &query.root {
        RelationalPlan::QueryBlock(block) => block.subqueries.len(),
        RelationalPlan::SetOp { subqueries, .. } | RelationalPlan::Values { subqueries, .. } => {
            subqueries.len()
        }
    };
    match &query.root {
        RelationalPlan::QueryBlock(_) => {
            if let Some(mut slot) = query.source_slot()? {
                let value = input
                    .source(slot.take(), SourceScope::Queries, control)?
                    .await?;
                slot.restore(value);
            }
        }
        RelationalPlan::SetOp { .. } => {
            for child in [QueryChildSlot::Left, QueryChildSlot::Right] {
                checkpoint(control, input).await?;
                let mut slot = query.query_slot(child);
                let value = input.query(slot.take(), control)?.await?;
                slot.restore(value);
            }
        }
        RelationalPlan::Values { .. } => {}
    }
    for index in 0..subqueries {
        checkpoint(control, input).await?;
        let mut slot = query.query_slot(QueryChildSlot::Subquery(index));
        let value = input.query(slot.take(), control)?.await?;
        slot.restore(value);
    }
    if let RelationalPlan::QueryBlock(block) = &query.root {
        let resolved =
            crate::plan::windows::resolved_window_definitions_from(block, control, input.work())
                .await?;
        let resolved = control
            .finish(resolved, control.empty_reservation())?
            .into_shared(control)?;
        roots::normalize(query, resolved, control, input).await?;
    }
    query.prune_subqueries_in_place(control)
}

async fn command<I: Input>(
    command: &mut MutableCommandPlan,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    command.assert_owner(control);
    checkpoint(control, input).await?;
    for index in 0..command.ctes().len() {
        body(command.cte_slot(index), control, input).await?;
    }
    if let Some(mut slot) = command.source_slot() {
        let value = input
            .source(slot.take(), SourceScope::Queries, control)?
            .await?;
        slot.restore(value);
    }
    let (insert, subqueries) = match &**command {
        CommandPlan::Insert(plan) => (plan.source.is_some(), plan.subqueries.len()),
        CommandPlan::Update(plan) => (false, plan.subqueries.len()),
        CommandPlan::Delete(plan) => (false, plan.subqueries.len()),
        CommandPlan::Merge(plan) => (false, plan.subqueries.len()),
        _ => (false, 0),
    };
    if insert {
        command_query(command, CommandQuerySlot::InsertSource, control, input).await?;
    }
    for index in 0..subqueries {
        command_query(command, CommandQuerySlot::Subquery(index), control, input).await?;
    }
    match &**command {
        CommandPlan::CreateView { .. }
        | CommandPlan::CreateMaterializedView { .. }
        | CommandPlan::CreateTableAs { .. }
        | CommandPlan::DeclareCursor { .. } => {
            command_query(command, CommandQuerySlot::Body, control, input).await?;
        }
        CommandPlan::Explain { .. } | CommandPlan::Prepare { .. } => {
            body(command.executable_slot()?, control, input).await?;
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            let count = params.len();
            for parameter in 0..count {
                let (call, queries) = match &**command {
                    CommandPlan::Call { args, .. } => (true, args[parameter].subqueries.len()),
                    CommandPlan::Execute { params, .. } => {
                        (false, params[parameter].subqueries.len())
                    }
                    _ => unreachable!(),
                };
                for query in 0..queries {
                    let slot = if call {
                        CommandQuerySlot::CallArgumentSubquery {
                            argument: parameter,
                            query,
                        }
                    } else {
                        CommandQuerySlot::ExecuteParameterSubquery { parameter, query }
                    };
                    command_query(command, slot, control, input).await?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn command_query<I: Input>(
    command: &mut MutableCommandPlan,
    selected: CommandQuerySlot,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    if let Some(mut slot) = command.query_slot(selected) {
        let value = input.query(slot.take(), control)?.await?;
        slot.restore(value);
    }
    Ok(())
}

async fn source<I: Input>(
    source: &mut MutableSourcePlan,
    scope: SourceScope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    source.assert_owner(control);
    checkpoint(control, input).await?;
    if let SourceScope::Roots(resolved) = scope {
        return roots::source(source, resolved, control, input).await;
    }
    match &**source {
        SourcePlan::Subquery { .. } => {
            if let Some(mut slot) = source.query_slot()? {
                let value = input.query(slot.take(), control)?.await?;
                slot.restore(value);
            }
        }
        SourcePlan::Join { .. } => {
            for child in [SourceChildSlot::Left, SourceChildSlot::Right] {
                let mut slot = source.child_slot(child);
                let value = input
                    .source(slot.take(), SourceScope::Queries, control)?
                    .await?;
                slot.restore(value);
            }
        }
        _ => {}
    }
    Ok(())
}
