//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete command copies retain private field ownership through consuming edits.

use super::{memory::RewriteMemory, MutableQueryPlan};
use crate::{
    plan::{copy::Copying, CommandPlan, OwnedScalarExpr},
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod assignments;
mod children;
pub use assignments::CommandAssignmentSlot;
pub use children::CommandQuerySlot;

#[derive(Debug)]
pub struct MutableCommandPlan {
    pub(super) command: CommandPlan,
    pub(super) memory: Option<RewriteMemory>,
}

#[derive(Clone, Copy, Debug)]
pub enum CommandScalarSlot {
    Assignment(usize),
    Predicate,
    Returning(usize),
    InsertValue { row: usize, column: usize },
    MergeJoin,
    MergeCondition(usize),
    MergeValue { clause: usize, value: usize },
    ConflictPredicate,
    CallArgument(usize),
    ExecuteParameter(usize),
}

impl CommandPlan {
    /// Copy complete command syntax while retaining independently replaceable executable fields. Utility syntax remains with this command owner and is not evaluated by copying.
    pub fn copy_for_rewrite(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableCommandPlan, SQLError> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            return Ok(MutableCommandPlan {
                command: self.clone(),
                memory: None,
            });
        };
        let command = copying.command_syntax(self)?;
        let output = MutableCommandPlan {
            command,
            memory: Some(copying.into_rewrite_memory()),
        };
        control.check()?;
        Ok(output)
    }
}

impl MutableCommandPlan {
    pub fn reserved_bytes(&self) -> usize {
        self.memory
            .as_ref()
            .map_or(0, RewriteMemory::reserved_bytes)
    }

    /// Rewrite one scalar root without changing the statement's other inputs or its diagnostic order.
    pub fn map_scalar(
        mut self,
        slot: CommandScalarSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(expression) = slot.expression_mut(&mut self.command) else {
            return Ok(self);
        };
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| slot.memory_mut(memory).take())
            .map(|memory| match memory {
                RewriteMemory::Scalar(memory) => memory,
                RewriteMemory::Fields { .. } => unreachable!("command scalar owner"),
            });
        let input = OwnedScalarExpr::from_rewrite_parts(
            std::mem::replace(expression, ScalarExpr::Literal(Value::Null)),
            memory,
        );
        let output = input.rewrite(control, map)?;
        let (expression, memory) = output.into_rewrite_parts();
        *slot
            .expression_mut(&mut self.command)
            .expect("original command scalar") = expression;
        if let Some(owner) = &mut self.memory {
            *slot.memory_mut(owner) = memory.map(RewriteMemory::Scalar);
        }
        Ok(self)
    }

    /// Rewrite a retained query body while preserving its original enclosing box. EXPLAIN option interpretation and utility execution remain with their existing owners.
    pub fn map_query_body(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableQueryPlan) -> Result<MutableQueryPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let query = match &mut self.command {
            CommandPlan::DeclareCursor { query, .. }
            | CommandPlan::CreateTableAs { query, .. }
            | CommandPlan::CreateMaterializedView { query, .. }
            | CommandPlan::CreateView { query, .. } => query,
            _ => {
                return Err(SQLError::Internal(
                    "command has no directly retained query body".into(),
                ))
            }
        };
        let input = MutableQueryPlan {
            query: std::mem::replace(query.as_mut(), super::queries::empty_query()),
            memory: self.memory.as_mut().and_then(|memory| {
                memory
                    .field_mut("query")
                    .as_mut()
                    .expect("command query field")
                    .field_mut("query")
                    .take()
            }),
        };
        let output = map(input)?;
        output.assert_owner(control);
        control.check()?;
        **query = output.query;
        if let Some(memory) = &mut self.memory {
            *memory
                .field_mut("query")
                .as_mut()
                .expect("command query field")
                .field_mut("query") = output.memory;
        }
        Ok(self)
    }

    pub fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CommandPlan>, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Self { command, memory } = self;
        Ok(control.finish(command, memory.map(RewriteMemory::into_reservation))?)
    }

    pub(super) fn assert_owner(&self, control: &ProductionControl<'_>) {
        match (&self.memory, control.budget()) {
            (Some(RewriteMemory::Fields { direct, .. }), Some(budget)) => assert!(
                direct.budget().shares_allowance(budget),
                "command rewrite preserves its constructor allowance"
            ),
            (None, None) => {}
            _ => panic!("controlled and ordinary command ownership must not be mixed"),
        }
    }
}

impl std::ops::Deref for MutableCommandPlan {
    type Target = CommandPlan;
    fn deref(&self) -> &CommandPlan {
        &self.command
    }
}

impl CommandScalarSlot {
    fn expression_mut(self, command: &mut CommandPlan) -> Option<&mut ScalarExpr> {
        match (self, command) {
            (Self::Assignment(index), CommandPlan::Update(plan)) => {
                Some(&mut plan.assignments[index].value)
            }
            (Self::Predicate, CommandPlan::Update(plan)) => plan.predicate.as_mut(),
            (Self::Predicate, CommandPlan::Delete(plan)) => plan.predicate.as_mut(),
            (Self::Returning(index), CommandPlan::Insert(plan)) => {
                Some(&mut plan.returning[index].expr)
            }
            (Self::Returning(index), CommandPlan::Update(plan)) => {
                Some(&mut plan.returning[index].expr)
            }
            (Self::Returning(index), CommandPlan::Delete(plan)) => {
                Some(&mut plan.returning[index].expr)
            }
            (Self::Returning(index), CommandPlan::Merge(plan)) => {
                Some(&mut plan.returning[index].expr)
            }
            (Self::InsertValue { row, column }, CommandPlan::Insert(plan)) => {
                Some(&mut plan.rows[row][column])
            }
            (Self::MergeJoin, CommandPlan::Merge(plan)) => Some(&mut plan.join_condition),
            (Self::MergeCondition(index), CommandPlan::Merge(plan)) => {
                use crate::plan::MergeWhenPlan;
                match &mut plan.when_clauses[index] {
                    MergeWhenPlan::UpdateMatched { condition, .. }
                    | MergeWhenPlan::UpdateNotMatchedBySource { condition, .. }
                    | MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                    | MergeWhenPlan::InsertNotMatched { condition, .. }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition }
                    | MergeWhenPlan::NothingNotMatchedBySource { condition } => condition.as_mut(),
                }
            }
            (Self::MergeValue { clause, value }, CommandPlan::Merge(plan)) => {
                let crate::plan::MergeWhenPlan::InsertNotMatched { values, .. } =
                    &mut plan.when_clauses[clause]
                else {
                    panic!("MERGE clause has insertion values");
                };
                Some(&mut values[value])
            }
            (Self::ConflictPredicate, CommandPlan::Insert(plan)) => {
                let conflict = plan.on_conflict.as_mut()?;
                let crate::plan::ConflictActionPlan::Update { predicate, .. } =
                    &mut conflict.action
                else {
                    return None;
                };
                predicate.as_deref_mut()
            }
            (Self::CallArgument(index), CommandPlan::Call { args, .. }) => {
                Some(&mut args[index].scalar)
            }
            (Self::ExecuteParameter(index), CommandPlan::Execute { params, .. }) => {
                Some(&mut params[index].scalar)
            }
            _ => panic!("command scalar selector matches its command kind"),
        }
    }

    fn memory_mut(self, memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
        let input = match self {
            Self::CallArgument(index) => return item(memory, "args", index).scalar_mut(),
            Self::ExecuteParameter(index) => return item(memory, "params", index).scalar_mut(),
            _ => memory.field_mut("value0").as_mut().expect("mutation body"),
        };
        match self {
            Self::Assignment(index) => item(input, "assignments", index)
                .field_mut("value")
                .as_mut()
                .expect("assignment value")
                .scalar_mut(),
            Self::Predicate => input
                .field_mut("predicate")
                .as_mut()
                .expect("mutation predicate")
                .scalar_mut(),
            Self::Returning(index) => item(input, "returning", index).scalar_mut(),
            Self::InsertValue { row, column } => item(input, "rows", row)
                .item_mut(column)
                .as_mut()
                .expect("insert cell")
                .scalar_mut(),
            Self::MergeJoin => input
                .field_mut("join_condition")
                .as_mut()
                .expect("MERGE join")
                .scalar_mut(),
            Self::MergeCondition(index) => item(input, "when_clauses", index)
                .field_mut("condition")
                .as_mut()
                .expect("MERGE condition")
                .scalar_mut(),
            Self::MergeValue { clause, value } => {
                item(item(input, "when_clauses", clause), "values", value).scalar_mut()
            }
            Self::ConflictPredicate => input
                .field_mut("on_conflict")
                .as_mut()
                .expect("conflict")
                .field_mut("action")
                .as_mut()
                .expect("conflict action")
                .field_mut("predicate")
                .as_mut()
                .expect("conflict predicate")
                .scalar_mut(),
            Self::CallArgument(_) | Self::ExecuteParameter(_) => {
                unreachable!("utility argument selected above")
            }
        }
    }
}

fn item<'a>(memory: &'a mut RewriteMemory, name: &str, index: usize) -> &'a mut RewriteMemory {
    memory
        .field_mut(name)
        .as_mut()
        .expect("command vector")
        .item_mut(index)
        .as_mut()
        .expect("command vector item")
}
