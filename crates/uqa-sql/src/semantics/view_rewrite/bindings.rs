//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and native view callers lend the same inherited CTE namespace.

use crate::{
    binding::{
        production::{
            BindingMap, BindingNameSet, ControlledBindingContext, ControlledBindingSnapshot,
        },
        snapshot::BindingSnapshot,
    },
    SQLError,
};
use uqa_core::memory::ProductionControl;

#[derive(Clone, Copy)]
pub enum ViewRewriteBindings<'a> {
    Ordinary(&'a BindingSnapshot),
    Controlled(&'a ControlledBindingSnapshot),
}

impl ViewRewriteBindings<'_> {
    pub(super) fn inherit(
        self,
        scope: &mut ControlledBindingContext<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match self {
            Self::Ordinary(parent) => {
                scope.ctes = BindingMap::new();
                scope.deferred_ctes = BindingMap::new();
                scope.non_returning_ctes = BindingNameSet::new();
                for (name, schema) in &parent.ctes {
                    scope
                        .ctes
                        .insert_with_control(name, schema.clone(), control)?;
                }
                for (name, plan) in &parent.deferred_ctes {
                    scope
                        .deferred_ctes
                        .insert_with_control(name, plan.clone(), control)?;
                }
                for name in &parent.non_returning_ctes {
                    scope
                        .non_returning_ctes
                        .insert_with_control(name, control)?;
                }
            }
            Self::Controlled(parent) => {
                scope.ctes = parent
                    .ctes
                    .copy_with_control(control, |schema| Ok(schema.clone()))?;
                scope.deferred_ctes = parent
                    .deferred_ctes
                    .copy_with_control(control, |plan| Ok(plan.clone()))?;
                scope.non_returning_ctes = parent.non_returning_ctes.copy_with_control(control)?;
            }
        }
        Ok(())
    }
}
