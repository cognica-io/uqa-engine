//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A detached child keeps its original payload and constructor records while the parent retains the allocated slot.

use super::{
    memory::RewriteMemory, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
    MutableSourcePlan, OwnedScalarExpr,
};
use crate::{
    plan::{
        retirement::{
            construction::{Root, Workspace},
            empty_query, empty_source,
        },
        scalar::ScalarMemory,
        CommandPlan, QueryPlan, SourcePlan,
    },
    ScalarExpr,
};

pub(super) trait Editable: Sized {
    type Value;
    type Memory;
    fn empty() -> Self::Value;
    fn from_parts(value: Self::Value, memory: Option<Self::Memory>) -> Self;
    fn into_parts(self) -> (Self::Value, Option<Self::Memory>);
}

pub(super) struct Slot<'a, T: Editable> {
    value: Location<'a, T::Value>,
    memory: Option<&'a mut Option<T::Memory>>,
}

enum Location<'a, T> {
    Value(&'a mut T),
    Optional(&'a mut Option<T>),
}

impl<'a, T: Editable> Slot<'a, T> {
    pub(super) fn new(value: &'a mut T::Value, memory: Option<&'a mut Option<T::Memory>>) -> Self {
        Self {
            value: Location::Value(value),
            memory,
        }
    }

    pub(super) fn optional(
        value: &'a mut Option<T::Value>,
        memory: Option<&'a mut Option<T::Memory>>,
    ) -> Self {
        Self {
            value: Location::Optional(value),
            memory,
        }
    }

    pub(super) fn take(&mut self) -> T {
        T::from_parts(
            match &mut self.value {
                Location::Value(value) => std::mem::replace(value, T::empty()),
                Location::Optional(value) => value.take().expect("present child slot"),
            },
            self.memory.as_mut().and_then(|memory| memory.take()),
        )
    }

    pub(super) fn restore(self, value: T) {
        let (value, memory) = value.into_parts();
        match self.value {
            Location::Value(slot) => *slot = value,
            Location::Optional(slot) => *slot = Some(value),
        }
        if let Some(slot) = self.memory {
            *slot = memory;
        }
    }
}

pub(super) enum BodySlot<'a> {
    Query(Slot<'a, MutableQueryPlan>),
    Command(Slot<'a, MutableCommandPlan>),
}

impl BodySlot<'_> {
    pub(super) fn take(&mut self) -> MutableExecutablePlan {
        match self {
            Self::Query(slot) => MutableExecutablePlan::Query(slot.take()),
            Self::Command(slot) => MutableExecutablePlan::Command(slot.take()),
        }
    }

    pub(super) fn complete(
        self,
        value: MutableExecutablePlan,
        control: &uqa_core::memory::ProductionControl<'_>,
        kind: &str,
    ) -> Result<(), crate::SQLError> {
        match (self, value) {
            (Self::Query(slot), MutableExecutablePlan::Query(value)) => {
                value.assert_owner(control);
                control.check()?;
                slot.restore(value);
            }
            (Self::Command(slot), MutableExecutablePlan::Command(value)) => {
                value.assert_owner(control);
                control.check()?;
                slot.restore(value);
            }
            (Self::Query(_), MutableExecutablePlan::Command(_)) => {
                return Err(crate::SQLError::Internal(format!(
                    "{kind} rewrite changed a query body into a command"
                )));
            }
            (Self::Command(_), MutableExecutablePlan::Query(_)) => {
                return Err(crate::SQLError::Internal(format!(
                    "{kind} rewrite changed a command body into a query"
                )));
            }
        }
        Ok(())
    }
}

macro_rules! plan {
    ($mutable:ty, $raw:ty, $field:ident, $empty:expr) => {
        impl Editable for $mutable {
            type Value = $raw;
            type Memory = RewriteMemory;
            fn empty() -> Self::Value {
                $empty
            }
            fn from_parts(value: Self::Value, memory: Option<Self::Memory>) -> Self {
                Self {
                    $field: value,
                    memory,
                }
            }
            fn into_parts(self) -> (Self::Value, Option<Self::Memory>) {
                (self.$field, self.memory)
            }
        }
        impl Root for $mutable {
            fn retire(self, workspace: &mut Workspace) {
                self.$field.retire(workspace);
                drop(self.memory);
            }
        }
    };
}
plan!(MutableQueryPlan, QueryPlan, query, empty_query());
plan!(MutableSourcePlan, SourcePlan, source, empty_source());
plan!(MutableCommandPlan, CommandPlan, command, CommandPlan::Empty);

impl Root for MutableExecutablePlan {
    fn retire(self, workspace: &mut Workspace) {
        match self {
            Self::Query(value) => value.retire(workspace),
            Self::Command(value) => value.retire(workspace),
        }
    }
}

impl Editable for OwnedScalarExpr {
    type Value = ScalarExpr;
    type Memory = ScalarMemory;
    fn empty() -> Self::Value {
        ScalarExpr::Literal(uqa_core::Value::Null)
    }
    fn from_parts(value: Self::Value, memory: Option<Self::Memory>) -> Self {
        Self::from_rewrite_parts(value, memory)
    }
    fn into_parts(self) -> (Self::Value, Option<Self::Memory>) {
        self.into_rewrite_parts()
    }
}
