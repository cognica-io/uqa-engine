//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consuming scoped visitation keeps constructor ownership and admitted lexical names through command and source rewrites.

use super::{
    CommandQuerySlot, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
    MutableSourcePlan, QueryChildSlot, SourceChildSlot,
};
use crate::{
    plan::{CommandPlan, RelationalPlan, SourcePlan},
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub trait OwnedScopedPlanVisitor {
    fn query_complete(&mut self, query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        Ok(query)
    }
    fn command(
        &mut self,
        command: MutableCommandPlan,
        _outer: &dyn Fn(&str) -> bool,
    ) -> Result<(MutableCommandPlan, bool), SQLError> {
        Ok((command, true))
    }
    fn source(
        &mut self,
        source: MutableSourcePlan,
        _visible: &dyn Fn(&str) -> bool,
    ) -> Result<(MutableSourcePlan, bool), SQLError> {
        Ok((source, true))
    }
}

mod input;
mod scope;
use super::edits::BodySlot;
use crate::catalog::work::{self, Work};
use input::Input;
use scope::Scope;

impl MutableExecutablePlan {
    pub fn visit_scoped(
        self,
        control: &ProductionControl<'_>,
        visitor: &mut dyn OwnedScopedPlanVisitor,
    ) -> Result<Self, SQLError> {
        self.visit_scoped_in(control, &|_| false, visitor)
    }

    pub(crate) fn visit_scoped_in(
        mut self,
        control: &ProductionControl<'_>,
        outer: &dyn Fn(&str) -> bool,
        visitor: &mut dyn OwnedScopedPlanVisitor,
    ) -> Result<Self, SQLError> {
        let mut input = input::Immediate::new(outer, visitor);
        work::inline(plan(&mut self, Scope::root(), control, &mut input))?;
        Ok(self)
    }
}

async fn checkpoint<I: Input>(
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    control.check()?;
    input.work().checkpoint().await?;
    control.check()?;
    Ok(())
}

async fn plan<I: Input>(
    value: &mut MutableExecutablePlan,
    scope: Scope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    match value {
        MutableExecutablePlan::Query(value) => query(value, scope, control, input).await,
        MutableExecutablePlan::Command(value) => command(value, scope, control, input).await,
    }
}

async fn body<I: Input>(
    slot: BodySlot<'_>,
    scope: Scope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    match slot {
        BodySlot::Query(mut slot) => {
            let value = input.query(slot.take(), scope, control)?.await?;
            slot.restore(value);
        }
        BodySlot::Command(mut slot) => {
            let value = input.command(slot.take(), scope, control)?.await?;
            slot.restore(value);
        }
    }
    Ok(())
}

async fn query<I: Input>(
    value: &mut MutableQueryPlan,
    outer: Scope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    value.assert_owner(control);
    checkpoint(control, input).await?;
    let (scope, recursive) = outer.extend(&value.ctes, control, input.work()).await?;
    let result = async {
        for index in 0..value.ctes.len() {
            body(
                value.cte_slot(index),
                scope.before(index, recursive),
                control,
                input,
            )
            .await?;
        }
        match &value.root {
            RelationalPlan::QueryBlock(_) => {
                if let Some(mut slot) = value.source_slot()? {
                    let value = input.source(slot.take(), scope.clone(), control)?.await?;
                    slot.restore(value);
                }
            }
            RelationalPlan::SetOp { .. } => {
                for child in [QueryChildSlot::Left, QueryChildSlot::Right] {
                    checkpoint(control, input).await?;
                    let mut slot = value.query_slot(child);
                    let value = input.query(slot.take(), scope.clone(), control)?.await?;
                    slot.restore(value);
                }
            }
            RelationalPlan::Values { .. } => {}
        }
        let count = match &value.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::SetOp { subqueries, .. }
            | RelationalPlan::Values { subqueries, .. } => subqueries.len(),
        };
        for index in 0..count {
            checkpoint(control, input).await?;
            let mut slot = value.query_slot(QueryChildSlot::Subquery(index));
            let value = input.query(slot.take(), scope.clone(), control)?.await?;
            slot.restore(value);
        }
        input.query_complete(value, control)?.await
    }
    .await;
    let retired = scope.retire(control, input.work()).await;
    result.and(retired)
}

async fn source<I: Input>(
    value: &mut MutableSourcePlan,
    scope: Scope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    value.assert_owner(control);
    checkpoint(control, input).await?;
    if !input.source_enter(value, &scope, control)?.await? {
        return Ok(());
    }
    match &**value {
        SourcePlan::Subquery { .. } => {
            if let Some(mut slot) = value.query_slot()? {
                let value = input.query(slot.take(), scope, control)?.await?;
                slot.restore(value);
            }
        }
        SourcePlan::Table {
            name,
            executable_query: Some(_),
            ..
        } => {
            let cte = match work::reference(name, control, input.work()).await? {
                Some(reference) if reference.schema.is_none() => {
                    let local = work::decoded_name(reference.name, control, input.work()).await?;
                    input.visible(&scope, &local, control).await?
                }
                _ => false,
            };
            if let Some(mut slot) = value.query_slot()? {
                let value = input
                    .query(
                        slot.take(),
                        if cte { scope } else { Scope::default() },
                        control,
                    )?
                    .await?;
                slot.restore(value);
            }
        }
        SourcePlan::Join { .. } => {
            for child in [SourceChildSlot::Left, SourceChildSlot::Right] {
                checkpoint(control, input).await?;
                let mut slot = value.child_slot(child);
                let value = input.source(slot.take(), scope.clone(), control)?.await?;
                slot.restore(value);
            }
        }
        _ => {}
    }
    Ok(())
}

async fn command_query<I: Input>(
    value: &mut MutableCommandPlan,
    selected: CommandQuerySlot,
    scope: &Scope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    if let Some(mut slot) = value.query_slot(selected) {
        let value = input.query(slot.take(), scope.clone(), control)?.await?;
        slot.restore(value);
    }
    Ok(())
}

async fn command<I: Input>(
    value: &mut MutableCommandPlan,
    outer: Scope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    value.assert_owner(control);
    checkpoint(control, input).await?;
    if !input.command_enter(value, &outer, control)?.await? {
        return Ok(());
    }
    match &**value {
        CommandPlan::CreateView { .. }
        | CommandPlan::CreateTableAs { .. }
        | CommandPlan::CreateMaterializedView { .. }
        | CommandPlan::DeclareCursor { .. } => {
            return command_query(value, CommandQuerySlot::Body, &outer, control, input).await;
        }
        CommandPlan::Explain { .. } | CommandPlan::Prepare { .. } => {
            return body(value.executable_slot()?, outer, control, input).await;
        }
        CommandPlan::Call { args: params, .. } | CommandPlan::Execute { params, .. } => {
            let count = params.len();
            for parameter in 0..count {
                checkpoint(control, input).await?;
                let (call, queries) = match &**value {
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
                    command_query(value, slot, &outer, control, input).await?;
                }
            }
            return Ok(());
        }
        CommandPlan::Insert(_)
        | CommandPlan::Update(_)
        | CommandPlan::Delete(_)
        | CommandPlan::Merge(_) => {}
        _ => return Ok(()),
    }
    let (scope, recursive) = outer.extend(value.ctes(), control, input.work()).await?;
    let result = async {
        for index in 0..value.ctes().len() {
            body(
                value.cte_slot(index),
                scope.before(index, recursive),
                control,
                input,
            )
            .await?;
        }
        if let Some(mut slot) = value.source_slot() {
            let value = input.source(slot.take(), scope.clone(), control)?.await?;
            slot.restore(value);
        }
        if matches!(&**value, CommandPlan::Insert(insert) if insert.source.is_some()) {
            command_query(
                value,
                CommandQuerySlot::InsertSource,
                &scope,
                control,
                input,
            )
            .await?;
        }
        for index in 0..value.scalar_subqueries().len() {
            command_query(
                value,
                CommandQuerySlot::Subquery(index),
                &scope,
                control,
                input,
            )
            .await?;
        }
        Ok(())
    }
    .await;
    let retired = scope.retire(control, input.work()).await;
    result.and(retired)
}
