//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A stored unresolved call observes only the declaration lookups reached at its original SQL placement.

use super::StoredFunctionSite;
use crate::{
    semantics::{sets::validation::builtin_returns_set, windows::BuiltinWindowFunction},
    SQLError,
};
use uqa_core::memory::ProductionControl;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationFunctionKind {
    Scalar,
    Table,
    Aggregate,
}

impl StoredFunctionSite {
    /// A stored SQL object, builtin, structural operation or deferred binding error already supplies the call's authority. Only unresolved expression calls reach application lookup.
    pub fn with_stored_binding(
        self,
        binding: Option<&crate::ast::FunctionBinding>,
    ) -> Option<Self> {
        let selected = binding.is_some_and(|binding| {
            binding.object_id.is_some()
                || binding.builtin
                || binding.dispatch.is_some()
                || binding.value_binding.is_some()
                || binding.resolution_error.is_some()
        });
        (!selected).then_some(self)
    }

    /// Observe positive and negative application lookups up to the first selected registration. The callback returns whether that kind is installed and must never invoke the application function.
    pub fn visit_application_lookups(
        self,
        name: &str,
        control: &ProductionControl<'_>,
        mut installed: impl FnMut(ApplicationFunctionKind) -> bool,
    ) -> Result<(), SQLError> {
        use ApplicationFunctionKind::{Aggregate, Scalar, Table};
        match self {
            Self::From => {
                let local =
                    crate::semantics::builtin_function_dispatch_name_with_control(name, control)?;
                if !builtin_returns_set(&local)
                    && !crate::registry::is_operator_join_table_function(&local)
                {
                    installed(Table);
                }
            }
            Self::Window => {
                let local =
                    crate::semantics::builtin_function_dispatch_name_with_control(name, control)?;
                if BuiltinWindowFunction::from_dispatch_name(&local).is_none() {
                    installed(Aggregate);
                }
            }
            Self::Domain => {
                installed(Scalar);
            }
            Self::Expression if crate::semantics::is_builtin_aggregate_call(name, None) => {
                installed(Aggregate);
            }
            Self::Expression => {
                if !installed(Aggregate) && !installed(Table) {
                    installed(Scalar);
                }
            }
        }
        Ok(())
    }
}
