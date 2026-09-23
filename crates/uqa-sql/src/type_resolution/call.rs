//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Call binding transfers the existing expression allocations and new constructors through one produced owner.

use crate::{ast::FunctionBinding, ColumnType, SQLError, ScalarExpr};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

/// Every moved Func field keeps its incoming reservation until its replacement is returned or destroyed, including decorations whose payloads may share that lease.
#[derive(Debug)]
pub(super) struct BindingCall {
    pub(super) name: String,
    pub(super) binding: Option<FunctionBinding>,
    pub(super) arguments: Vec<ScalarExpr>,
    pub(super) distinct: bool,
    pub(super) order_by: Vec<crate::ScalarOrder>,
    pub(super) filter: Option<Box<ScalarExpr>>,
}

/// The central type resolver supplies this callback so auxiliary binding constructors share the existing inference semantics and resource scope.
pub(super) type InferType<'a> =
    dyn FnMut(&ScalarExpr) -> Result<Option<Produced<ColumnType>>, SQLError> + 'a;

/// A standalone call wrapper drops every field before its complete lease. In-place consumers may instead borrow a larger root expression owner's lease while siblings remain alive.
pub(super) struct CallOwner {
    pub(super) call: BindingCall,
    pub(super) memory: Option<MemoryReservation>,
}

impl CallOwner {
    pub(super) fn new(
        call: Produced<BindingCall>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let call = check_owner(call, control)?;
        let (call, memory) = call.into_parts();
        Ok(Self { call, memory })
    }
    pub(super) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<BindingCall>, SQLError> {
        Ok(control.finish(self.call, self.memory)?)
    }
}

/// Validate a borrowed root lease without moving it: cancellation or invariant failure must leave sibling payloads charged until their root owner drops them.
pub(super) fn check_memory(
    memory: Option<&MemoryReservation>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    match (control.budget(), memory) {
        (Some(budget), Some(memory)) => assert!(
            budget.shares_allowance(memory.budget()),
            "binding uses a foreign allowance"
        ),
        (None, None) => {}
        _ => panic!("binding ownership mode differs from its control"),
    }
    control.check()?;
    Ok(())
}

pub(super) fn check_owner<T>(
    value: Produced<T>,
    control: &ProductionControl<'_>,
) -> Result<Produced<T>, SQLError> {
    let (value, memory) = value.into_parts();
    Ok(control.finish(value, memory)?)
}

pub(super) fn infer_with_control(
    expression: &ScalarExpr,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    infer(expression)?
        .map(|ty| check_owner(ty, control))
        .transpose()
}

#[cfg(test)]
mod tests;
