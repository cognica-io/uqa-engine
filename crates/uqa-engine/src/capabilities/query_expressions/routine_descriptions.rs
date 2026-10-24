//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native metadata generations supply identity; SQL and Execution own description reuse.

use super::ScopedEngineHook;
use std::sync::{Arc, Weak};
use uqa_core::{catalog_role::RoleIdentity, memory::ProductionControl, Value};
use uqa_execution::routines::invocation::{self, context::RoutineCallRetention};
use uqa_sql::{
    ast::FunctionBinding,
    routines::{
        invocation::descriptions::{RoutineCallDescriptions, RoutineDescriptionScope},
        lifecycle::RoutineRegistry,
    },
    SQLError,
};

pub(super) struct NativeRoutineScope {
    revision: u64,
    registry: Weak<RoutineRegistry>,
    role: RoleIdentity,
    identity: RoutineDescriptionScope,
}

impl ScopedEngineHook<'_> {
    fn routine_description_scope(&self) -> Option<RoutineDescriptionScope> {
        let revision = self.engine.runtime.regtype_output_cache.revision();
        let registry = self
            .engine
            .query_sql_function_snapshots
            .clone()
            .unwrap_or_else(|| self.engine.durable.sql_user_functions.snapshot());
        let role = self
            .engine
            .session
            .state
            .read()
            .authorization
            .current()
            .identity();
        let saved = self.routine_scope.get_or_init(|| NativeRoutineScope {
            revision,
            registry: Arc::downgrade(&registry),
            role,
            identity: RoutineDescriptionScope::default(),
        });
        (saved.revision == revision
            && saved.role == role
            && Weak::ptr_eq(&saved.registry, &Arc::downgrade(&registry)))
        .then(|| saved.identity.clone())
    }

    pub(super) fn call_described_routine(
        &self,
        binding: &FunctionBinding,
        args: &[(Option<String>, Value)],
        descriptions: &RoutineCallDescriptions,
        control: &ProductionControl<'_>,
    ) -> Option<Result<Value, SQLError>> {
        let scope = || self.routine_description_scope();
        let context = self.engine.routine_invocation_context();
        let retained = RoutineCallRetention {
            descriptions,
            scope: &scope,
            control: *control,
        };
        invocation::call_bound_user_scalar_function_with_descriptions(
            &context, binding, args, retained,
        )
    }
}

#[cfg(test)]
mod tests;
