//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Arena-aware edits finish nested query namespaces before copying occurrences into their parent.

use super::super::analysis::QueryObserver;
use super::super::{
    nodes, CommandQuerySlot, MutableExecutablePlan, MutableQueryPlan, OwnedScalarExpr,
    QueryChildSlot, RewriteMemory, SourceChildSlot,
};
use super::owner::ScalarQueryArena;
use crate::{
    plan::{
        CommandPlan, CtePlan, CtePlanBody, ExpressionPlan, QueryPlan, RelationalPlan, SourcePlan,
        UnifiedPlan,
    },
    SQLError, ScalarExpr,
};
use std::ptr::NonNull;
use uqa_core::memory::ProductionControl;

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
        let mut walker = Walker {
            control,
            transform,
            complete,
        };
        match self {
            Self::Query(query) => {
                query.assert_owner(control);
                walker.query(&mut query.query, query.memory.as_mut())
            }
            Self::Command(command) => {
                command.assert_owner(control);
                walker.command(&mut command.command, command.memory.as_mut())
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

struct Walker<'a, 'c, 't, 'q> {
    control: &'a ProductionControl<'c>,
    transform: &'a mut ScalarArenaTransform<'t>,
    complete: &'a mut QueryObserver<'q>,
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

impl Walker<'_, '_, '_, '_> {
    fn executable(
        &mut self,
        plan: &mut UnifiedPlan,
        memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        match plan {
            UnifiedPlan::Query(query) => self.query(query, field(memory, "query")),
            UnifiedPlan::Command(command) => self.command(command, field(memory, "command")),
        }
    }

    fn query(
        &mut self,
        query: &mut QueryPlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        self.ctes(&mut query.ctes, field(memory.as_deref_mut(), "ctes"))?;
        match &mut query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &mut block.from {
                    let owner = memory.as_deref_mut().map(|memory| {
                        super::super::sources::query_source_memory(memory)
                            .as_mut()
                            .expect("query source")
                    });
                    self.source_children(source, owner)?;
                }
            }
            RelationalPlan::SetOp { .. } => {
                for slot in [QueryChildSlot::Left, QueryChildSlot::Right] {
                    let owner = memory
                        .as_deref_mut()
                        .map(|memory| slot.memory_mut(memory).as_mut().expect("set query"));
                    self.query(slot.query_mut(query), owner)?;
                }
            }
            RelationalPlan::Values { .. } => {}
        }
        let count = query_arena(&mut query.root).len();
        for index in 0..count {
            let slot = QueryChildSlot::Subquery(index);
            let owner = memory
                .as_deref_mut()
                .map(|memory| slot.memory_mut(memory).as_mut().expect("scalar query"));
            self.query(slot.query_mut(query), owner)?;
        }
        let mut arena = ScalarQueryArena {
            queries: std::mem::take(query_arena(&mut query.root)),
            memory: memory
                .as_deref_mut()
                .and_then(|memory| query_memory(memory).take()),
        };
        let control = self.control;
        let transform = &mut self.transform;
        let mut action = |expression: &mut ScalarExpr,
                          memory: Option<&mut nodes::ScalarMemory>,
                          control: &ProductionControl<'_>| {
            OwnedScalarExpr::transform_nodes_in_place(
                expression,
                memory,
                control,
                &mut |identity, expression| transform(identity, expression, &mut arena),
            )
        };
        if let RelationalPlan::QueryBlock(block) = &mut query.root {
            if let Some(source) = &mut block.from {
                let owner = memory.as_deref_mut().map(|memory| {
                    super::super::sources::query_source_memory(memory)
                        .as_mut()
                        .expect("query source")
                });
                nodes::local_source(source, owner, control, &mut action)?;
            }
        }
        nodes::query_scalars(query, memory.as_deref_mut(), false, control, &mut action)?;
        *query_arena(&mut query.root) = arena.queries;
        if let Some(memory) = memory.as_deref_mut() {
            *query_memory(memory) = arena.memory;
        }
        (self.complete)(query)?.install(query, memory, control)
    }

    fn source_children(
        &mut self,
        source: &mut SourcePlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        match source {
            SourcePlan::Join { left, right, .. } => {
                self.source_children(
                    left,
                    memory.as_deref_mut().map(|memory| {
                        SourceChildSlot::Left
                            .memory_mut(memory)
                            .as_mut()
                            .expect("left source")
                    }),
                )?;
                self.source_children(
                    right,
                    memory.map(|memory| {
                        SourceChildSlot::Right
                            .memory_mut(memory)
                            .as_mut()
                            .expect("right source")
                    }),
                )?;
            }
            SourcePlan::Subquery { body, .. } => {
                self.query(body, field(field(memory, "body"), "query"))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn ctes(
        &mut self,
        ctes: &mut [CtePlan],
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        for (index, cte) in ctes.iter_mut().enumerate() {
            self.control.check()?;
            let mut owner = item(memory.as_deref_mut(), index);
            let body = field(owner.as_deref_mut(), "body");
            match &mut cte.body {
                CtePlanBody::Query(query) => self.query(query, field(body, "query"))?,
                CtePlanBody::Command(command) => self.command(command, field(body, "command"))?,
            }
            if let Some(cycle) = &mut cte.cycle {
                let mut owner = field(owner, "cycle");
                self.scalar_without_arena(
                    &mut cycle.mark_value,
                    field(owner.as_deref_mut(), "mark_value"),
                )?;
                self.scalar_without_arena(&mut cycle.mark_default, field(owner, "mark_default"))?;
            }
        }
        Ok(())
    }

    fn scalar_without_arena(
        &mut self,
        expression: &mut ScalarExpr,
        memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        let mut arena = ScalarQueryArena {
            queries: Vec::new(),
            memory: None,
        };
        let memory = memory.map(|memory| nodes::scalar_memory(memory.scalar_mut()));
        OwnedScalarExpr::transform_nodes_in_place(
            expression,
            memory,
            self.control,
            &mut |identity, expression| (self.transform)(identity, expression, &mut arena),
        )
    }

    fn expression(
        &mut self,
        expression: &mut ExpressionPlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        for (index, query) in expression.subqueries.iter_mut().enumerate() {
            let owner = field(
                item(field(memory.as_deref_mut(), "subqueries"), index),
                "query",
            );
            self.query(query, owner)?;
        }
        let mut arena = ScalarQueryArena {
            queries: std::mem::take(&mut expression.subqueries),
            memory: memory
                .as_deref_mut()
                .and_then(|memory| memory.field_mut("subqueries").take()),
        };
        let owner = memory
            .as_deref_mut()
            .map(|memory| nodes::scalar_memory(memory.scalar_mut()));
        OwnedScalarExpr::transform_nodes_in_place(
            &mut expression.scalar,
            owner,
            self.control,
            &mut |identity, expression| (self.transform)(identity, expression, &mut arena),
        )?;
        expression.subqueries = arena.queries;
        if let Some(memory) = memory {
            *memory.field_mut("subqueries") = arena.memory;
        }
        Ok(())
    }

    fn command(
        &mut self,
        command: &mut CommandPlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        if let Some(ctes) = command.ctes_mut() {
            self.ctes(ctes, field(field(memory.as_deref_mut(), "value0"), "ctes"))?;
        }
        if let Some(source) = command.source_input_mut() {
            let owner = memory.as_deref_mut().map(|memory| {
                super::super::commands::children::source_memory(memory)
                    .as_mut()
                    .expect("command source")
            });
            self.source_children(source, owner)?;
        }
        if matches!(command, CommandPlan::Insert(_)) {
            self.command_query(
                command,
                memory.as_deref_mut(),
                CommandQuerySlot::InsertSource,
            )?;
        }
        for index in 0..command.scalar_subqueries().len() {
            self.command_query(
                command,
                memory.as_deref_mut(),
                CommandQuerySlot::Subquery(index),
            )?;
        }
        if let Some(queries) = command_arena(command) {
            let mut arena = ScalarQueryArena {
                queries: std::mem::take(queries),
                memory: memory.as_deref_mut().and_then(|memory| {
                    field(Some(memory), "value0")
                        .expect("mutation body")
                        .field_mut("subqueries")
                        .take()
                }),
            };
            let control = self.control;
            let transform = &mut self.transform;
            let mut local = |identity, expression| transform(identity, expression, &mut arena);
            super::super::commands::transformation::transform_local_command(
                command,
                memory.as_deref_mut(),
                control,
                &mut local,
            )?;
            if let Some(source) = command.source_input_mut() {
                let owner = memory.as_deref_mut().map(|memory| {
                    super::super::commands::children::source_memory(memory)
                        .as_mut()
                        .expect("command source")
                });
                nodes::transform_local_source(source, owner, control, &mut local)?;
            }
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
                self.query(query, field(field(memory, "query"), "query"))?;
            }
            CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
                self.executable(body, field(memory, "body"))?;
            }
            CommandPlan::Call { args, .. } => self.expressions(args, field(memory, "args"))?,
            CommandPlan::Execute { params, .. } => {
                self.expressions(params, field(memory, "params"))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn command_query(
        &mut self,
        command: &mut CommandPlan,
        memory: Option<&mut RewriteMemory>,
        slot: CommandQuerySlot,
    ) -> Result<(), SQLError> {
        if let Some(query) = slot.query_mut(command) {
            let owner =
                memory.map(|memory| slot.memory_mut(memory).as_mut().expect("command query"));
            self.query(query, owner)?;
        }
        Ok(())
    }

    fn expressions(
        &mut self,
        expressions: &mut [ExpressionPlan],
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        for (index, expression) in expressions.iter_mut().enumerate() {
            self.expression(expression, item(memory.as_deref_mut(), index))?;
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
        RewriteMemory::Scalar(_) => unreachable!(),
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
