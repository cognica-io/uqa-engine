//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Arena-aware edits finish nested query namespaces before copying occurrences into their parent.

use super::super::analysis::QueryObserver;
use super::super::{
    nodes, CommandQuerySlot, MutableExecutablePlan, MutableQueryPlan, OwnedScalarExpr,
    QueryChildSlot, RewriteMemory,
};
use super::owner::ScalarQueryArena;
use crate::catalog::work::inline;
use crate::{
    plan::{CommandPlan, ExpressionPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan},
    SQLError, ScalarExpr,
};
use input::{Immediate, Input};
use nodes::transformation::{ctes_from, input::checkpoint};
use std::ptr::NonNull;
use uqa_core::memory::ProductionControl;

mod input;

pub type ScalarArenaTransform<'a> = dyn FnMut(
        NonNull<ScalarExpr>,
        OwnedScalarExpr,
        &mut ScalarQueryArena,
    ) -> Result<OwnedScalarExpr, SQLError>
    + 'a;

impl MutableExecutablePlan {
    /// Rewrite existing nodes at their analysis addresses while lending the node's own query arena. Every nested arena is completed before its parent, and copies added by a callback are already analyzed occurrences.
    pub fn transform_scalar_arenas(
        mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarArenaTransform<'_>,
    ) -> Result<Self, SQLError> {
        self.transform_scalar_arenas_in_place(control, transform)?;
        Ok(self)
    }

    pub(crate) fn transform_scalar_arenas_in_place(
        &mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarArenaTransform<'_>,
    ) -> Result<(), SQLError> {
        self.transform_scalar_arenas_with_metadata(control, transform, &mut |_| {
            Ok(crate::plan::QueryMetadataUpdate::default())
        })
    }

    /// Complete query metadata before an enclosing expression can copy the analyzed query into another occurrence.
    pub(crate) fn transform_scalar_arenas_with_metadata(
        &mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarArenaTransform<'_>,
        complete: &mut QueryObserver<'_>,
    ) -> Result<(), SQLError> {
        let mut input = Immediate::new(transform, complete);
        let mut walker = Walker {
            control,
            input: &mut input,
        };
        match self {
            Self::Query(query) => {
                query.assert_owner(control);
                inline(walker.query(&mut query.query, query.memory.as_mut()))
            }
            Self::Command(command) => {
                command.assert_owner(control);
                inline(walker.command(&mut command.command, command.memory.as_mut()))
            }
        }
    }
}

impl MutableQueryPlan {
    pub fn transform_scalar_arenas(
        self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarArenaTransform<'_>,
    ) -> Result<Self, SQLError> {
        let MutableExecutablePlan::Query(query) =
            MutableExecutablePlan::Query(self).transform_scalar_arenas(control, transform)?
        else {
            unreachable!()
        };
        Ok(query)
    }
}

struct Walker<'a, 'c, I> {
    control: &'a ProductionControl<'c>,
    input: &'a mut I,
}

fn field<'a>(memory: Option<&'a mut RewriteMemory>, name: &str) -> Option<&'a mut RewriteMemory> {
    memory.map(|memory| {
        memory
            .field_mut(name)
            .as_mut()
            .expect("arena constructor field")
    })
}

fn item(memory: Option<&mut RewriteMemory>, index: usize) -> Option<&mut RewriteMemory> {
    memory.map(|memory| {
        memory
            .item_mut(index)
            .as_mut()
            .expect("arena constructor item")
    })
}

fn field_slot<'a>(
    memory: Option<&'a mut RewriteMemory>,
    name: &str,
) -> Option<&'a mut Option<RewriteMemory>> {
    memory.map(|memory| memory.field_mut(name))
}

impl<I: Input> Walker<'_, '_, I> {
    async fn executable(
        &mut self,
        plan: &mut UnifiedPlan,
        memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        match plan {
            UnifiedPlan::Query(query) => {
                self.input
                    .query(query, field_slot(memory, "query"), self.control)?
                    .await
            }
            UnifiedPlan::Command(command) => {
                self.input
                    .command(command, field_slot(memory, "command"), self.control)?
                    .await
            }
        }
    }

    async fn query(
        &mut self,
        query: &mut QueryPlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        checkpoint(self.control, self.input).await?;
        ctes_from(
            &mut query.ctes,
            field(memory.as_deref_mut(), "ctes"),
            self.control,
            self.input,
        )
        .await?;
        match &mut query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &mut block.from {
                    let owner = memory
                        .as_deref_mut()
                        .map(super::super::sources::query_source_memory);
                    self.input.source(source, owner, self.control)?.await?;
                }
            }
            RelationalPlan::SetOp { .. } => {
                for slot in [QueryChildSlot::Left, QueryChildSlot::Right] {
                    let owner = memory.as_deref_mut().map(|memory| slot.memory_mut(memory));
                    self.input
                        .query(slot.query_mut(query), owner, self.control)?
                        .await?;
                }
            }
            RelationalPlan::Values { .. } => {}
        }
        let count = query_arena(&mut query.root).len();
        for index in 0..count {
            let slot = QueryChildSlot::Subquery(index);
            let owner = memory.as_deref_mut().map(|memory| slot.memory_mut(memory));
            self.input
                .query(slot.query_mut(query), owner, self.control)?
                .await?;
        }
        self.input.enter_arena(ScalarQueryArena {
            queries: std::mem::take(query_arena(&mut query.root)),
            memory: memory
                .as_deref_mut()
                .and_then(|memory| query_memory(memory).take()),
        });
        if let RelationalPlan::QueryBlock(block) = &mut query.root {
            if let Some(source) = &mut block.from {
                let owner = memory
                    .as_deref_mut()
                    .map(super::super::sources::query_source_memory);
                self.input
                    .local_source(source, owner, self.control)?
                    .await?;
            }
        }
        nodes::roots::query(
            query,
            memory.as_deref_mut(),
            nodes::roots::QueryScope::Local,
            self.control,
            self.input,
        )
        .await?;
        let arena = self.input.leave_arena();
        *query_arena(&mut query.root) = arena.queries;
        if let Some(memory) = memory.as_deref_mut() {
            *query_memory(memory) = arena.memory;
        }
        self.input.complete(query, memory, self.control).await
    }

    async fn expression(
        &mut self,
        expression: &mut ExpressionPlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        for (index, query) in expression.subqueries.iter_mut().enumerate() {
            let owner = field_slot(
                item(field(memory.as_deref_mut(), "subqueries"), index),
                "query",
            );
            self.input.query(query, owner, self.control)?.await?;
        }
        self.input.enter_arena(ScalarQueryArena {
            queries: std::mem::take(&mut expression.subqueries),
            memory: memory
                .as_deref_mut()
                .and_then(|memory| memory.field_mut("subqueries").take()),
        });
        self.input
            .scalar(
                &mut expression.scalar,
                memory.as_deref_mut().map(|memory| memory.scalar_mut()),
                self.control,
            )
            .await?;
        let arena = self.input.leave_arena();
        expression.subqueries = arena.queries;
        if let Some(memory) = memory {
            *memory.field_mut("subqueries") = arena.memory;
        }
        Ok(())
    }

    async fn command(
        &mut self,
        command: &mut CommandPlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        checkpoint(self.control, self.input).await?;
        if let Some(ctes) = command.ctes_mut() {
            ctes_from(
                ctes,
                field(field(memory.as_deref_mut(), "value0"), "ctes"),
                self.control,
                self.input,
            )
            .await?;
        }
        if let Some(source) = command.source_input_mut() {
            let owner = memory
                .as_deref_mut()
                .map(super::super::commands::children::source_memory);
            self.input.source(source, owner, self.control)?.await?;
        }
        if matches!(command, CommandPlan::Insert(_)) {
            self.command_query(
                command,
                memory.as_deref_mut(),
                CommandQuerySlot::InsertSource,
            )
            .await?;
        }
        for index in 0..command.scalar_subqueries().len() {
            self.command_query(
                command,
                memory.as_deref_mut(),
                CommandQuerySlot::Subquery(index),
            )
            .await?;
        }
        if let Some(queries) = command_arena(command) {
            self.input.enter_arena(ScalarQueryArena {
                queries: std::mem::take(queries),
                memory: memory.as_deref_mut().and_then(|memory| {
                    field(Some(memory), "value0")
                        .expect("mutation body")
                        .field_mut("subqueries")
                        .take()
                }),
            });
            super::super::commands::transformation::command_from(
                command,
                memory.as_deref_mut(),
                self.control,
                self.input,
                false,
            )
            .await?;
            if let Some(source) = command.source_input_mut() {
                let owner = memory
                    .as_deref_mut()
                    .map(super::super::commands::children::source_memory);
                self.input
                    .local_source(source, owner, self.control)?
                    .await?;
            }
            let arena = self.input.leave_arena();
            *command_arena(command).expect("mutation arena") = arena.queries;
            if let Some(memory) = memory {
                *field(Some(memory), "value0")
                    .expect("mutation body")
                    .field_mut("subqueries") = arena.memory;
            }
            return Ok(());
        }
        match command {
            CommandPlan::CreateView { query, .. }
            | CommandPlan::CreateMaterializedView { query, .. }
            | CommandPlan::CreateTableAs { query, .. }
            | CommandPlan::DeclareCursor { query, .. } => {
                self.input
                    .query(
                        query,
                        field_slot(field(memory, "query"), "query"),
                        self.control,
                    )?
                    .await?;
            }
            CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
                self.executable(body, field(memory, "body")).await?;
            }
            CommandPlan::Call { args, .. } => self.expressions(args, field(memory, "args")).await?,
            CommandPlan::Execute { params, .. } => {
                self.expressions(params, field(memory, "params")).await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn command_query(
        &mut self,
        command: &mut CommandPlan,
        memory: Option<&mut RewriteMemory>,
        slot: CommandQuerySlot,
    ) -> Result<(), SQLError> {
        if let Some(query) = slot.query_mut(command) {
            let owner = memory.map(|memory| slot.memory_mut(memory));
            self.input.query(query, owner, self.control)?.await?;
        }
        Ok(())
    }

    async fn expressions(
        &mut self,
        expressions: &mut [ExpressionPlan],
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        for (index, expression) in expressions.iter_mut().enumerate() {
            checkpoint(self.control, self.input).await?;
            self.expression(expression, item(memory.as_deref_mut(), index))
                .await?;
        }
        Ok(())
    }
}

fn query_arena(root: &mut RelationalPlan) -> &mut Vec<QueryPlan> {
    match root {
        RelationalPlan::QueryBlock(block) => &mut block.subqueries,
        RelationalPlan::Values { subqueries, .. } | RelationalPlan::SetOp { subqueries, .. } => {
            subqueries
        }
    }
}

fn query_memory(memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
    let root = memory.field_mut("root").as_mut().expect("query root");
    let is_block = match root {
        RewriteMemory::Fields { fields, .. } => fields.iter().any(|field| field.name == "block"),
        RewriteMemory::Scalar(_) | RewriteMemory::Scalars { .. } => unreachable!(),
    };
    let root = if is_block {
        root.field_mut("block").as_mut().expect("query block")
    } else {
        root
    };
    root.field_mut("subqueries")
}

fn command_arena(command: &mut CommandPlan) -> Option<&mut Vec<QueryPlan>> {
    match command {
        CommandPlan::Insert(plan) => Some(&mut plan.subqueries),
        CommandPlan::Update(plan) => Some(&mut plan.subqueries),
        CommandPlan::Delete(plan) => Some(&mut plan.subqueries),
        CommandPlan::Merge(plan) => Some(&mut plan.subqueries),
        _ => None,
    }
}
