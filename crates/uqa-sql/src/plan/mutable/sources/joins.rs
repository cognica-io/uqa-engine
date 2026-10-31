//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join reconstruction transfers inputs while retaining the original allocated source boxes.

use super::super::super::copy::Copying;
use super::{
    empty_source, MutableSourcePlan, OwnedScalarExpr, RewriteMemory, SQLError, SourceChildSlot,
    SourcePlan, SourceScalarSlot,
};
use crate::{ast::JoinKind, plan::JoinExecutionStrategy};
use uqa_core::memory::ProductionControl;

#[derive(Debug)]
pub struct MutableJoinShell {
    source: MutableSourcePlan,
    predicate_slot: bool,
}

#[derive(Debug)]
pub struct MutableJoinParts {
    pub shell: MutableJoinShell,
    pub left: MutableSourcePlan,
    pub right: MutableSourcePlan,
    pub on: Option<OwnedScalarExpr>,
}

impl MutableSourcePlan {
    /// Split an undecorated inner/cross join into independent input owners and a shell retaining the two allocated boxes. The shell cannot be observed or published as executable syntax until assembly succeeds.
    pub fn split_join(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableJoinParts, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let SourcePlan::Join {
            left,
            right,
            on,
            kind: JoinKind::Inner | JoinKind::Cross,
            using: None,
            natural: false,
            alias: None,
            column_aliases,
            lateral: false,
            ..
        } = &mut self.source
        else {
            return Err(SQLError::Internal(
                "join decomposition requires an undecorated inner join".into(),
            ));
        };
        if !column_aliases.is_empty() {
            return Err(SQLError::Internal(
                "join decomposition cannot discard column aliases".into(),
            ));
        }
        let left = MutableSourcePlan {
            source: std::mem::replace(left.as_mut(), empty_source()),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| SourceChildSlot::Left.memory_mut(memory).take()),
        };
        let right = MutableSourcePlan {
            source: std::mem::replace(right.as_mut(), empty_source()),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| SourceChildSlot::Right.memory_mut(memory).take()),
        };
        let predicate_slot = on.is_some();
        let on = on.take().map(|expression| {
            let memory = self
                .memory
                .as_mut()
                .and_then(|memory| SourceScalarSlot::JoinOn.memory_mut(memory).take())
                .map(|memory| match memory {
                    RewriteMemory::Scalar(memory) => memory,
                    RewriteMemory::Fields { .. } => unreachable!("join predicate scalar owner"),
                });
            OwnedScalarExpr::from_rewrite_parts(expression, memory)
        });
        Ok(MutableJoinParts {
            shell: MutableJoinShell {
                source: self,
                predicate_slot,
            },
            left,
            right,
            on,
        })
    }
}

impl MutableJoinShell {
    /// Fill the retained boxes with selected inputs and publish an inner join. Inputs and predicate retain their own allowances; only a newly needed ON ownership record requires admission.
    pub fn assemble(
        mut self,
        left: MutableSourcePlan,
        right: MutableSourcePlan,
        on: Option<OwnedScalarExpr>,
        strategy: JoinExecutionStrategy,
        control: &ProductionControl<'_>,
    ) -> Result<MutableSourcePlan, SQLError> {
        self.source.assert_owner(control);
        left.assert_owner(control);
        right.assert_owner(control);
        control.check()?;
        let on = on
            .map(|on| on.rewrite(control, Ok::<_, SQLError>))
            .transpose()?;
        let mut new_predicate_owner = None;
        let (on, scalar_owner) = match on {
            Some(on) if !self.predicate_slot && control.budget().is_some() => {
                let mut copying =
                    Copying::for_rewrite(*control).expect("controlled join predicate");
                let expression = copying.take_scalar(on)?;
                new_predicate_owner = Some(copying.into_rewrite_memory());
                (Some(expression), None)
            }
            Some(on) => {
                let (expression, memory) = on.into_rewrite_parts();
                (Some(expression), memory.map(RewriteMemory::Scalar))
            }
            None => (None, None),
        };
        let SourcePlan::Join {
            comma,
            kind,
            left: left_box,
            right: right_box,
            on: predicate,
            strategy: selected,
            ..
        } = &mut self.source.source
        else {
            unreachable!("join shell")
        };
        **left_box = left.source;
        **right_box = right.source;
        *predicate = on;
        *comma = false;
        *kind = JoinKind::Inner;
        *selected = strategy;
        if let Some(memory) = &mut self.source.memory {
            *SourceChildSlot::Left.memory_mut(memory) = left.memory;
            *SourceChildSlot::Right.memory_mut(memory) = right.memory;
            if self.predicate_slot {
                *SourceScalarSlot::JoinOn.memory_mut(memory) = scalar_owner;
            } else if let Some(owner) = new_predicate_owner {
                *memory.field_mut("on") = Some(owner);
            }
        }
        control.check()?;
        Ok(self.source)
    }
}
