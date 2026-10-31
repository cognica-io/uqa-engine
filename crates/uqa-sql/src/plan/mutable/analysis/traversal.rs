//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata traverses the original query and source containers without extracting their syntax.

use super::super::{CommandQuerySlot, QueryChildSlot, SourceChildSlot};
use super::{MutableExecutablePlan, QueryPlan, RewriteMemory, SQLError, SourcePlan, Walker};
use crate::plan::{CommandPlan, CtePlan, CtePlanBody, RelationalPlan, UnifiedPlan};

fn field<'a>(memory: Option<&'a mut RewriteMemory>, name: &str) -> Option<&'a mut RewriteMemory> {
    memory.map(|memory| memory.field_mut(name).as_mut().expect("metadata field"))
}

impl Walker<'_, '_> {
    pub(super) fn plan(&mut self, plan: &mut MutableExecutablePlan) -> Result<(), SQLError> {
        match plan {
            MutableExecutablePlan::Query(query) => {
                query.assert_owner(&self.control);
                self.query(&mut query.query, query.memory.as_mut())
            }
            MutableExecutablePlan::Command(command) => {
                command.assert_owner(&self.control);
                self.command(&mut command.command, command.memory.as_mut())
            }
        }
    }

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

    pub(super) fn query(
        &mut self,
        query: &mut QueryPlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        self.install_query(query, memory.as_deref_mut())?;
        self.ctes(&mut query.ctes, field(memory.as_deref_mut(), "ctes"))?;
        match &mut query.root {
            RelationalPlan::QueryBlock(block) => {
                self.install_projection_aliases(
                    &mut block.projections,
                    field(
                        field(field(memory.as_deref_mut(), "root"), "block"),
                        "projections",
                    ),
                )?;
                if let Some(source) = &mut block.from {
                    let memory = memory.as_deref_mut().map(|memory| {
                        super::super::sources::query_source_memory(memory)
                            .as_mut()
                            .expect("query source")
                    });
                    self.source(source, memory)?;
                }
            }
            RelationalPlan::SetOp { .. } => {
                for slot in [QueryChildSlot::Left, QueryChildSlot::Right] {
                    self.query(
                        slot.query_mut(query),
                        memory
                            .as_deref_mut()
                            .map(|memory| slot.memory_mut(memory).as_mut().expect("set query")),
                    )?;
                }
            }
            RelationalPlan::Values { .. } => {}
        }
        let count = match &query.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::SetOp { subqueries, .. }
            | RelationalPlan::Values { subqueries, .. } => subqueries.len(),
        };
        for index in 0..count {
            let slot = QueryChildSlot::Subquery(index);
            self.query(
                slot.query_mut(query),
                memory
                    .as_deref_mut()
                    .map(|memory| slot.memory_mut(memory).as_mut().expect("scalar query")),
            )?;
        }
        Ok(())
    }

    fn source(
        &mut self,
        source: &mut SourcePlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        self.install_source(source, memory.as_deref_mut())?;
        match source {
            SourcePlan::Join { left, right, .. } => {
                self.source(
                    left,
                    memory.as_deref_mut().map(|memory| {
                        SourceChildSlot::Left
                            .memory_mut(memory)
                            .as_mut()
                            .expect("left source")
                    }),
                )?;
                self.source(
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
            SourcePlan::Table {
                executable_query: Some(body),
                ..
            } if self.query.is_some() || self.projections.is_some() => {
                self.query(body, field(field(memory, "executable_query"), "query"))?;
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
            let body = memory.as_deref_mut().map(|memory| {
                memory
                    .item_mut(index)
                    .as_mut()
                    .expect("CTE item")
                    .field_mut("body")
                    .as_mut()
                    .expect("CTE body")
            });
            match &mut cte.body {
                CtePlanBody::Query(query) => self.query(query, field(body, "query"))?,
                CtePlanBody::Command(command) => {
                    let enclosing = self.cte_command;
                    self.cte_command = true;
                    self.command(command, field(body, "command"))?;
                    self.cte_command = enclosing;
                }
            }
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
            self.query(
                query,
                memory.map(|memory| slot.memory_mut(memory).as_mut().expect("command query")),
            )?;
        }
        Ok(())
    }

    fn returning_aliases(
        &mut self,
        command: &mut CommandPlan,
        memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        let returning = match command {
            CommandPlan::Insert(plan) => Some(&mut plan.returning),
            CommandPlan::Update(plan) => Some(&mut plan.returning),
            CommandPlan::Delete(plan) => Some(&mut plan.returning),
            CommandPlan::Merge(plan) => Some(&mut plan.returning),
            _ => None,
        };
        if let Some(returning) = returning {
            self.install_projection_aliases(
                returning,
                field(field(memory, "value0"), "returning"),
            )?;
        }
        Ok(())
    }

    fn command(
        &mut self,
        command: &mut CommandPlan,
        mut memory: Option<&mut RewriteMemory>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        if self.mark_relations {
            match command {
                CommandPlan::Insert(plan) => {
                    plan.relations_bound = true;
                    if self.cte_command {
                        plan.target_relation_bound = true;
                    }
                }
                CommandPlan::Update(plan) => {
                    plan.relations_bound = true;
                    if self.cte_command {
                        plan.target_relation_bound = true;
                    }
                }
                CommandPlan::Delete(plan) => {
                    plan.relations_bound = true;
                    if self.cte_command {
                        plan.target_relation_bound = true;
                    }
                }
                _ => {}
            }
        }
        match command {
            CommandPlan::CreateView { .. }
            | CommandPlan::CreateTableAs { .. }
            | CommandPlan::CreateMaterializedView { .. }
            | CommandPlan::DeclareCursor { .. } => {
                return self.command_query(command, memory, CommandQuerySlot::Body);
            }
            CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
                return self.executable(body, field(memory, "body"));
            }
            CommandPlan::Call { args, .. } => {
                for argument in 0..args.len() {
                    let CommandPlan::Call { args, .. } = command else {
                        unreachable!()
                    };
                    for query in 0..args[argument].subqueries.len() {
                        self.command_query(
                            command,
                            memory.as_deref_mut(),
                            CommandQuerySlot::CallArgumentSubquery { argument, query },
                        )?;
                    }
                }
                return Ok(());
            }
            CommandPlan::Execute { params, .. } => {
                for parameter in 0..params.len() {
                    let CommandPlan::Execute { params, .. } = command else {
                        unreachable!()
                    };
                    for query in 0..params[parameter].subqueries.len() {
                        self.command_query(
                            command,
                            memory.as_deref_mut(),
                            CommandQuerySlot::ExecuteParameterSubquery { parameter, query },
                        )?;
                    }
                }
                return Ok(());
            }
            _ => {}
        }
        self.returning_aliases(command, memory.as_deref_mut())?;
        if let Some(ctes) = command.ctes_mut() {
            self.ctes(ctes, field(field(memory.as_deref_mut(), "value0"), "ctes"))?;
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
        if let Some(source) = command.source_input_mut() {
            let memory = memory.map(|memory| {
                super::super::commands::children::source_memory(memory)
                    .as_mut()
                    .expect("command source")
            });
            self.source(source, memory)?;
        }
        Ok(())
    }
}
