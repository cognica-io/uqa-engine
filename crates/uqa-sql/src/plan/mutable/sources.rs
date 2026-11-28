//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source edits preserve parent containers and independently owned descendants.

use super::{memory::RewriteMemory, MutableQueryPlan, RelationalPlan};
use crate::{
    plan::{OwnedScalarExpr, SourcePlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub(super) use crate::plan::retirement::empty_source;

mod bindings;
mod functions;
mod generated;
mod joins;
mod metadata;
mod parents;
mod references;
pub use joins::{MutableJoinParts, MutableJoinShell};

/// A source whose child and scalar constructor records follow consuming edits.
#[derive(Debug)]
pub struct MutableSourcePlan {
    pub(super) source: SourcePlan,
    pub(super) memory: super::RootMemory,
}

#[derive(Clone, Copy, Debug)]
pub enum SourceScalarSlot {
    JoinOn,
    Value { row: usize, column: usize },
    Argument(usize),
    GroupArgument { function: usize, argument: usize },
}

#[derive(Clone, Copy, Debug)]
pub enum SourceChildSlot {
    Left,
    Right,
}

impl MutableQueryPlan {
    /// Rewrite the complete FROM source without exposing a mutable query or separating a source from its constructor ownership.
    pub fn map_source(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableSourcePlan) -> Result<MutableSourcePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(mut slot) = self.source_slot()? else {
            return Ok(self);
        };
        let output = map(slot.take())?;
        output.assert_owner(control);
        control.check()?;
        slot.restore(output);
        Ok(self)
    }

    pub(super) fn source_slot(
        &mut self,
    ) -> Result<Option<super::edits::Slot<'_, MutableSourcePlan>>, SQLError> {
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "source rewrite requires a query block".into(),
            ));
        };
        if block.from.is_none() {
            return Ok(None);
        }
        Ok(Some(super::edits::Slot::optional(
            &mut block.from,
            self.memory.as_mut().map(query_source_memory),
        )))
    }
}

impl SourcePlan {
    /// Move a caller-owned range item through a consuming edit. An error leaves an empty source in the caller's slot.
    pub fn edit_uncontrolled(
        &mut self,
        edit: impl FnOnce(MutableSourcePlan) -> Result<MutableSourcePlan, SQLError>,
    ) -> Result<(), SQLError> {
        super::edits::edit_uncontrolled(self, edit)
    }
}

impl MutableSourcePlan {
    /// Move a caller-owned range item into the source editor.
    #[must_use]
    pub fn uncontrolled(source: SourcePlan) -> Self {
        Self {
            source,
            memory: None.into(),
        }
    }

    pub fn reserved_bytes(&self) -> usize {
        self.memory.reserved_bytes()
    }

    /// Rewrite a derived or attached table query while keeping its original source box and namespace metadata.
    pub fn map_query(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableQueryPlan) -> Result<MutableQueryPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(mut slot) = self.query_slot()? else {
            return Ok(self);
        };
        let input = slot.take();
        let output = map(input)?;
        output.assert_owner(control);
        control.check()?;
        slot.restore(output);
        Ok(self)
    }

    pub(super) fn query_slot(
        &mut self,
    ) -> Result<Option<super::edits::Slot<'_, MutableQueryPlan>>, SQLError> {
        let (query, field) = match &mut self.source {
            SourcePlan::Subquery { body, .. } => (body, "body"),
            SourcePlan::Table {
                executable_query: Some(query),
                ..
            } => (query, "executable_query"),
            SourcePlan::Table {
                executable_query: None,
                ..
            } => return Ok(None),
            _ => {
                return Err(SQLError::Internal(
                    "source has no directly retained query".into(),
                ));
            }
        };
        Ok(Some(super::edits::Slot::new(
            query.as_mut(),
            self.memory.as_mut().map(|memory| {
                memory
                    .field_mut(field)
                    .as_mut()
                    .expect("source query box")
                    .field_mut("query")
            }),
        )))
    }

    pub fn map_scalar(
        mut self,
        slot: SourceScalarSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(mut slot) = self.scalar_slot(slot) else {
            return Ok(self);
        };
        let input = slot.take();
        let output = input.rewrite(control, map)?;
        slot.restore(output);
        Ok(self)
    }

    pub(super) fn scalar_slot(
        &mut self,
        slot: SourceScalarSlot,
    ) -> Option<super::edits::Slot<'_, OwnedScalarExpr>> {
        Some(super::edits::Slot::new(
            slot.expression_mut(&mut self.source)?,
            self.memory.as_mut().map(|memory| slot.memory_mut(memory)),
        ))
    }

    pub fn map_child(
        mut self,
        slot: SourceChildSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(Self) -> Result<Self, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut slot = self.child_slot(slot);
        let input = slot.take();
        let output = map(input)?;
        output.assert_owner(control);
        control.check()?;
        slot.restore(output);
        Ok(self)
    }

    pub(super) fn child_slot(&mut self, slot: SourceChildSlot) -> super::edits::Slot<'_, Self> {
        super::edits::Slot::new(
            slot.source_mut(&mut self.source),
            self.memory.as_mut().map(|memory| slot.memory_mut(memory)),
        )
    }

    /// Select one join child and destroy the former parent, including the child's old enclosing box. The selected child's own allocations retain their separate constructor records.
    pub fn select_child(
        mut self,
        slot: SourceChildSlot,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let source = std::mem::replace(slot.source_mut(&mut self.source), empty_source());
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| slot.memory_mut(memory).take());
        drop(self);
        Ok(Self {
            source,
            memory: memory.into(),
        })
    }

    pub(super) fn assert_owner(&self, control: &ProductionControl<'_>) {
        match (self.memory.read().as_ref(), control.budget()) {
            (Some(RewriteMemory::Fields { direct, .. }), Some(budget)) => assert!(
                direct.budget().shares_allowance(budget),
                "source rewrite preserves its constructor allowance"
            ),
            (None, None) => {}
            _ => panic!("controlled and ordinary source ownership must not be mixed"),
        }
    }
}

impl std::ops::Deref for MutableSourcePlan {
    type Target = SourcePlan;
    fn deref(&self) -> &SourcePlan {
        &self.source
    }
}

impl SourceChildSlot {
    fn source_mut(self, source: &mut SourcePlan) -> &mut SourcePlan {
        let SourcePlan::Join { left, right, .. } = source else {
            panic!("join source child");
        };
        if matches!(self, Self::Left) {
            left
        } else {
            right
        }
    }

    pub(super) fn memory_mut(self, memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
        memory
            .field_mut(if matches!(self, Self::Left) {
                "left"
            } else {
                "right"
            })
            .as_mut()
            .expect("source box owner")
            .field_mut("source")
    }
}

impl SourceScalarSlot {
    pub(super) fn expression_mut(self, source: &mut SourcePlan) -> Option<&mut ScalarExpr> {
        match (self, source) {
            (Self::JoinOn, SourcePlan::Join { on, .. }) => on.as_mut(),
            (Self::Value { row, column }, SourcePlan::Values { rows, .. }) => {
                Some(&mut rows[row][column])
            }
            (Self::Argument(index), SourcePlan::Function { args, .. }) => Some(&mut args[index]),
            (
                Self::GroupArgument { function, argument },
                SourcePlan::FunctionGroup { functions, .. },
            ) => Some(&mut functions[function].args[argument]),
            _ => panic!("source scalar selector matches its source kind"),
        }
    }

    pub(super) fn memory_mut(
        self,
        memory: &mut RewriteMemory,
    ) -> &mut Option<crate::plan::scalar::ScalarMemory> {
        match self {
            Self::JoinOn => memory
                .field_mut("on")
                .as_mut()
                .expect("join predicate")
                .scalar_mut(),
            Self::Value { row, column } => item(memory, "rows", row).scalar_item_mut(column),
            Self::Argument(index) => item(memory, "args", index).scalar_mut(),
            Self::GroupArgument { function, argument } => {
                item(item(memory, "functions", function), "args", argument).scalar_mut()
            }
        }
    }
}

pub(super) fn query_source_memory(memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
    memory
        .field_mut("root")
        .as_mut()
        .expect("query root")
        .field_mut("block")
        .as_mut()
        .expect("query block")
        .field_mut("from")
        .as_mut()
        .expect("FROM field")
        .field_mut("source")
}

fn item<'a>(memory: &'a mut RewriteMemory, name: &str, index: usize) -> &'a mut RewriteMemory {
    memory
        .field_mut(name)
        .as_mut()
        .expect("source vector")
        .item_mut(index)
        .as_mut()
        .expect("source vector item")
}
