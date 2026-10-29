//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command children retain their enclosing command boxes and local query arenas.

use super::super::{sources::empty_source, MutableSourcePlan};
use super::{item, CommandPlan, MutableCommandPlan, MutableQueryPlan, RewriteMemory, SQLError};
use crate::plan::QueryPlan;
use uqa_core::memory::ProductionControl;

#[derive(Clone, Copy, Debug)]
pub enum CommandQuerySlot {
    Body,
    InsertSource,
    Subquery(usize),
    CallArgumentSubquery { argument: usize, query: usize },
    ExecuteParameterSubquery { parameter: usize, query: usize },
}

impl MutableCommandPlan {
    pub fn map_query_child(
        self,
        slot: CommandQuerySlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableQueryPlan) -> Result<MutableQueryPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.map_query_child_with_parent(slot, control, |_, child| map(child))
    }

    /// Rewrite a command-owned query while observing the command's unchanged namespace. The selected query temporarily becomes empty in its original container.
    pub fn map_query_child_with_parent(
        mut self,
        slot: CommandQuerySlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&CommandPlan, MutableQueryPlan) -> Result<MutableQueryPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(query) = slot.query_mut(&mut self.command) else {
            return Ok(self);
        };
        let input = MutableQueryPlan {
            query: std::mem::replace(query, super::super::queries::empty_query()),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| slot.memory_mut(memory).take()),
        };
        let output = map(&self.command, input)?;
        output.assert_owner(control);
        control.check()?;
        *slot
            .query_mut(&mut self.command)
            .expect("original command query") = output.query;
        if let Some(memory) = &mut self.memory {
            *slot.memory_mut(memory) = output.memory;
        }
        Ok(self)
    }

    pub fn map_source(
        self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableSourcePlan) -> Result<MutableSourcePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.map_source_with_parent(control, |_, source| map(source))
    }

    /// Rewrite UPDATE/DELETE/MERGE's source without copying its command-owned box or scalar siblings.
    pub fn map_source_with_parent(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&CommandPlan, MutableSourcePlan) -> Result<MutableSourcePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(source) = self.command.source_input_mut() else {
            return Ok(self);
        };
        let source = std::mem::replace(source, empty_source());
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| source_memory(memory).take());
        let output = map(&self.command, MutableSourcePlan { source, memory })?;
        output.assert_owner(control);
        control.check()?;
        *self
            .command
            .source_input_mut()
            .expect("original command source") = output.source;
        if let Some(memory) = &mut self.memory {
            *source_memory(memory) = output.memory;
        }
        Ok(self)
    }
}

impl CommandQuerySlot {
    fn query_mut(self, command: &mut CommandPlan) -> Option<&mut QueryPlan> {
        match (self, command) {
            (
                Self::Body,
                CommandPlan::DeclareCursor { query, .. }
                | CommandPlan::CreateTableAs { query, .. }
                | CommandPlan::CreateMaterializedView { query, .. }
                | CommandPlan::CreateView { query, .. },
            ) => Some(query),
            (Self::InsertSource, CommandPlan::Insert(plan)) => plan.source.as_deref_mut(),
            (Self::Subquery(index), CommandPlan::Insert(plan)) => Some(&mut plan.subqueries[index]),
            (Self::Subquery(index), CommandPlan::Update(plan)) => Some(&mut plan.subqueries[index]),
            (Self::Subquery(index), CommandPlan::Delete(plan)) => Some(&mut plan.subqueries[index]),
            (Self::Subquery(index), CommandPlan::Merge(plan)) => Some(&mut plan.subqueries[index]),
            (Self::CallArgumentSubquery { argument, query }, CommandPlan::Call { args, .. }) => {
                Some(&mut args[argument].subqueries[query])
            }
            (
                Self::ExecuteParameterSubquery { parameter, query },
                CommandPlan::Execute { params, .. },
            ) => Some(&mut params[parameter].subqueries[query]),
            _ => panic!("command query selector matches its command kind"),
        }
    }

    fn memory_mut(self, memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
        let field = match self {
            Self::Body => memory
                .field_mut("query")
                .as_mut()
                .expect("command query body"),
            Self::InsertSource => memory
                .field_mut("value0")
                .as_mut()
                .expect("INSERT body")
                .field_mut("source")
                .as_mut()
                .expect("INSERT source"),
            Self::Subquery(index) => item(
                memory.field_mut("value0").as_mut().expect("mutation body"),
                "subqueries",
                index,
            ),
            Self::CallArgumentSubquery { argument, query } => {
                item(item(memory, "args", argument), "subqueries", query)
            }
            Self::ExecuteParameterSubquery { parameter, query } => {
                item(item(memory, "params", parameter), "subqueries", query)
            }
        };
        field.field_mut("query")
    }
}

fn source_memory(memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
    memory
        .field_mut("value0")
        .as_mut()
        .expect("mutation body")
        .field_mut("source")
        .as_mut()
        .expect("mutation source box")
        .field_mut("source")
}
