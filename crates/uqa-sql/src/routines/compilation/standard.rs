//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-standard bodies lower one statement at a time and resolve collected role constants after the last statement has compiled.

use super::{
    lower_sql_routine_statement, validate_routine_signature, RoutineCompilationContext,
    SQLRoutineLowering,
};
use crate::{
    ast::{CreateFunction, FunctionBody, Statement},
    binding::RoutineParameterScope,
    catalog::regrole_dependencies::{regrole_constant_error, StoredRegroleConstants},
    plan::{copy::capture_stored_with_footprint, UnifiedPlan},
    routines::{CompiledFunctionBody, RetainedRoutineValue, SharedRoutineBody},
    SQLError, SQLParam,
};
use uqa_core::memory::{
    MemoryError, MemoryReservation, Produced, ProductionControl, ProductionVec,
};

pub(super) struct StatementCompiler {
    positional: Produced<Vec<SQLParam>>,
    parameters: RoutineParameterScope,
}

impl StatementCompiler {
    pub(super) fn new(
        context: &RoutineCompilationContext<'_>,
        def: &CreateFunction,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let positional = crate::routines::body_validation::routine_parameter_values_with_control(
            context.types,
            def,
            control,
        )?;
        let parameters = crate::routines::body_parameters::sql_body_parameter_scope_with_control(
            def,
            &positional,
            control,
        )?;
        Ok(Self {
            positional,
            parameters,
        })
    }

    pub(super) fn compile(
        &self,
        context: &RoutineCompilationContext<'_>,
        statement: Statement,
        lowering: SQLRoutineLowering,
    ) -> Result<UnifiedPlan, SQLError> {
        let mut plan = lower_sql_routine_statement(context, statement, lowering)?;
        // SQL-standard syntax keeps definition-time names. A source body is analyzed in the namespace of its first execution.
        if lowering.bind_catalog_dependencies {
            let binding = context.catalog.binding_snapshot()?;
            crate::binding::bind_routine_parameter_references(
                context.routines,
                &mut plan,
                &self.positional,
                &binding.context(),
                &self.parameters,
            )?;
            self.bind_calls(context, &mut plan, &binding.context(), lowering)?;
        }
        Ok(plan)
    }

    fn bind_calls(
        &self,
        context: &RoutineCompilationContext<'_>,
        plan: &mut UnifiedPlan,
        binding: &crate::binding::BindingContext<'_>,
        lowering: SQLRoutineLowering,
    ) -> Result<(), SQLError> {
        if let UnifiedPlan::Query(query) = plan {
            let origin = if lowering.persisted_definition {
                crate::binding::stored_routines::StoredCallOrigin::Persisted
            } else {
                crate::binding::stored_routines::StoredCallOrigin::Definition
            };
            origin.bind_query(context.routines, query, &self.positional, binding, None)?;
        }
        Ok(())
    }
}

/// The source cardinality is known at admission. Neither output can grow beyond one entry per statement plus the signature's role inputs.
fn slots<T>(
    count: usize,
    control: &ProductionControl<'_>,
) -> Result<(Vec<T>, Option<MemoryReservation>), SQLError> {
    let mut values = ProductionVec::new(*control);
    values.reserve(count)?;
    Ok(values.finish()?.into_parts())
}

pub struct SQLStandardBodyCompilation {
    compiler: StatementCompiler,
    lowering: SQLRoutineLowering,
    statement_count: usize,
    plans: Vec<UnifiedPlan>,
    memory: Option<MemoryReservation>,
    children: usize,
    roles: Vec<Produced<StoredRegroleConstants>>,
    role_memory: Option<MemoryReservation>,
    checked_roles: usize,
}

impl SQLStandardBodyCompilation {
    pub fn new(
        context: &RoutineCompilationContext<'_>,
        definition: &CreateFunction,
        persisted: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let signature = validate_routine_signature(context, definition)?;
        let FunctionBody::Statements(statements) = &definition.body else {
            return Err(SQLError::Internal(
                "expected a SQL-standard routine body".into(),
            ));
        };
        let compiler = StatementCompiler::new(context, definition, control)?;
        let (plans, memory) = slots(statements.len(), control)?;
        let mut state = Self {
            compiler,
            lowering: SQLRoutineLowering {
                bind_catalog_dependencies: true,
                persisted_definition: persisted,
                preserve_target_expressions: false,
            },
            statement_count: statements.len(),
            plans,
            memory,
            children: 0,
            roles: Vec::new(),
            role_memory: None,
            checked_roles: 0,
        };
        state.collect_roles(signature, control)?;
        Ok(state)
    }

    fn collect_roles(
        &mut self,
        roles: StoredRegroleConstants,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if roles.has_constants() {
            if self.roles.is_empty() {
                let count = self
                    .statement_count
                    .checked_add(1)
                    .ok_or(MemoryError::SizeOverflow)?;
                (self.roles, self.role_memory) = slots(count, control)?;
            }
            self.roles.push(roles.retain(control)?);
        }
        Ok(())
    }

    pub fn next_statement(&self) -> Option<usize> {
        (self.plans.len() < self.statement_count).then_some(self.plans.len())
    }

    /// Complete the ordinary binding and stored projection of the privately constructed statement. The original constructor allowance remains attached until that projection is installed.
    pub fn accept_retained(
        &mut self,
        context: &RoutineCompilationContext<'_>,
        plan: crate::plan::RetainedExecutablePlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        use crate::plan::MutableExecutablePlan;
        control.check()?;
        let plan = match plan.into_mutable() {
            MutableExecutablePlan::Query(query) => {
                let namespace = context.catalog.stored_query_namespace();
                let names = super::StoredQueryBindingContext {
                    relations: context.relations,
                    lookup_mode: super::RelationLookupMode::Bound,
                    sequences: context.sequences,
                    temporary_schema: &namespace.temporary_schema,
                    transition_relations: &namespace.transition_relations,
                };
                let (query, _) = names.bind_query_relation_names_with_control(
                    query,
                    "SQL routine body",
                    false,
                    true,
                    control,
                )?;
                MutableExecutablePlan::Query(names.bind_query_sequences_with_control(
                    query,
                    "SQL routine body",
                    true,
                    control,
                )?)
            }
            command => command.mark_catalog_relations_bound(control)?,
        };
        let snapshot = context.catalog.binding_snapshot()?;
        let binding = snapshot.context();
        let plan = crate::binding::bind_routine_parameter_references_owned(
            context.routines,
            plan,
            &self.compiler.positional,
            binding.copy_with_control(control)?,
            &self.compiler.parameters,
            control,
        )?;
        let (mut plan, lease) = plan.finish(control)?.into_parts();
        let result = (|| {
            self.compiler
                .bind_calls(context, &mut plan, &binding, self.lowering)?;
            self.record_plan(plan, control)
        })();
        drop(lease);
        result
    }

    fn record_plan(
        &mut self,
        mut plan: UnifiedPlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let mut roles = StoredRegroleConstants::default();
        roles.collect_plan(&mut plan);
        self.collect_roles(roles, control)?;
        if control.budget().is_some() {
            let (retained, children) = capture_stored_with_footprint(&plan, control)?;
            self.children = self
                .children
                .checked_add(children)
                .ok_or(MemoryError::SizeOverflow)?;
            let (retained, memory) = retained.into_parts();
            self.memory = control.combine(self.memory.take(), memory);
            plan = retained;
        }
        self.plans.push(plan);
        Ok(())
    }

    /// Compile one original statement or validate one previously collected group of role inputs. A later statement's analysis error precedes deferred role diagnostics.
    pub fn advance(
        &mut self,
        context: &RoutineCompilationContext<'_>,
        statements: &[Statement],
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        if statements.len() != self.statement_count {
            return Err(SQLError::Internal(
                "routine statement list changed during compilation".into(),
            ));
        }
        if let Some(statement) = statements.get(self.plans.len()) {
            let plan = self
                .compiler
                .compile(context, statement.clone(), self.lowering)?;
            self.record_plan(plan, control)?;
            return Ok(false);
        }
        if let Some(roles) = self.roles.get(self.checked_roles) {
            roles.validate_inputs_with(context.regroles)?;
            self.checked_roles += 1;
            return Ok(false);
        }
        if self.roles.is_empty() {
            Ok(true)
        } else {
            Err(regrole_constant_error())
        }
    }

    pub(crate) fn into_ordinary(self) -> CompiledFunctionBody {
        debug_assert!(self.memory.is_none());
        CompiledFunctionBody::SQL(self.plans)
    }

    pub fn into_shared(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<SharedRoutineBody, SQLError> {
        if self.plans.len() != self.statement_count || !self.roles.is_empty() {
            return Err(SQLError::Internal(
                "SQL-standard compilation is incomplete".into(),
            ));
        }
        Ok(RetainedRoutineValue::native_with_children(
            control.finish(CompiledFunctionBody::SQL(self.plans), self.memory)?,
            self.children,
            control,
        )?)
    }
}
