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
    plan::{copy::stored_plan::BoundPlan, UnifiedPlan},
    routines::{CompiledFunctionBody, RetainedRoutineValue, SharedRoutineBody},
    SQLError, SQLParam,
};

mod retention;
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
    pending: Option<retention::Statement>,
    plans: Vec<UnifiedPlan>,
    memory: Option<MemoryReservation>,
    children: usize,
    roles: Vec<Produced<StoredRegroleConstants>>,
    role_memory: Option<MemoryReservation>,
    checked_roles: usize,
    next_role_input: usize,
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
        let mut state = Self::for_body(context, definition, persisted, control)?;
        if signature.has_constants() {
            state.reserve_roles(control)?;
            state.roles.push(signature.retain(control)?);
        }
        Ok(state)
    }

    /// Prepare a stored SQL-standard body after consuming its signature through the same private catalog and work window as the body.
    pub async fn new_from_input(
        catalog: &impl super::StoredRoutineCatalog,
        definition: &CreateFunction,
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        let signature = crate::routines::declaration::validate_stored_routine_signature_from(
            catalog, definition, control, input,
        )
        .await?;
        let mut state = Self::for_body(
            &RoutineCompilationContext::from_catalog(catalog),
            definition,
            true,
            control,
        )?;
        if signature.has_constants() {
            state.reserve_roles(control)?;
            state.roles.push(signature);
        }
        Ok(state)
    }

    fn for_body(
        context: &RoutineCompilationContext<'_>,
        definition: &CreateFunction,
        persisted: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let FunctionBody::Statements(statements) = &definition.body else {
            return Err(SQLError::Internal(
                "expected a SQL-standard routine body".into(),
            ));
        };
        let compiler = StatementCompiler::new(context, definition, control)?;
        let (plans, memory) = slots(statements.len(), control)?;
        Ok(Self {
            compiler,
            lowering: SQLRoutineLowering {
                bind_catalog_dependencies: true,
                persisted_definition: persisted,
                preserve_target_expressions: false,
            },
            statement_count: statements.len(),
            pending: None,
            plans,
            memory,
            children: 0,
            roles: Vec::new(),
            role_memory: None,
            checked_roles: 0,
            next_role_input: 0,
        })
    }

    fn reserve_roles(&mut self, control: &ProductionControl<'_>) -> Result<(), SQLError> {
        if self.roles.is_empty() {
            let count = self
                .statement_count
                .checked_add(1)
                .ok_or(MemoryError::SizeOverflow)?;
            (self.roles, self.role_memory) = slots(count, control)?;
        }
        Ok(())
    }

    pub fn next_statement(&self) -> Option<usize> {
        (self.pending.is_none() && self.plans.len() < self.statement_count)
            .then_some(self.plans.len())
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
        let (plan, workspace) = plan.into_mutable();
        let plan = match plan {
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
        let mut plan = BoundPlan::new(plan.finish(control)?, Some(workspace));
        self.compiler
            .bind_calls(context, &mut plan, &binding, self.lowering)?;
        self.record_plan(plan, control)
    }

    pub async fn accept_retained_from(
        &mut self,
        catalog: &impl super::StoredRoutineCatalog,
        plan: crate::plan::RetainedExecutablePlan,
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        let (mut plan, snapshot) = super::bind_stored_statement_from(
            catalog,
            plan,
            &self.compiler.positional,
            &self.compiler.parameters,
            control,
            input,
        )
        .await?;
        if let UnifiedPlan::Query(query) = &mut *plan {
            let storage = crate::binding::stored_routines::StoredCallOrigin::Persisted
                .bind_query_from(
                    catalog,
                    query,
                    &self.compiler.positional,
                    snapshot.context_with_control(control)?,
                    control,
                    input,
                )
                .await?;
            plan.retain_storage(storage);
        }
        self.record_plan(plan, control)
    }

    fn record_plan(
        &mut self,
        plan: BoundPlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if control.budget().is_some() {
            self.pending = Some(retention::Statement::new(
                plan,
                retention::Stage::Roles,
                control,
            )?);
        } else {
            let roles = crate::catalog::work::inline(StoredRegroleConstants::from_plan(
                (&*plan).into(),
                control,
                &mut (),
            ))?;
            if roles.has_constants() {
                self.reserve_roles(control)?;
                self.roles.push(roles);
            }
            self.plans.push(plan.into_ordinary());
        }
        Ok(())
    }

    fn advance_pending(&mut self, control: &ProductionControl<'_>) -> Result<(), SQLError> {
        let pending = self.pending.as_mut().expect("accepted statement");
        match pending.advance(control)? {
            None => {}
            Some(retention::Completed::Roles(roles, source)) => {
                if roles.has_constants() {
                    self.reserve_roles(control)?;
                    self.roles.push(roles);
                }
                self.pending = Some(retention::Statement::new(
                    source,
                    retention::Stage::Fields,
                    control,
                )?);
            }
            Some(retention::Completed::Plan(plan, children)) => {
                self.children = self
                    .children
                    .checked_add(children)
                    .ok_or(MemoryError::SizeOverflow)?;
                let (plan, memory) = plan.into_parts();
                self.memory = control.combine(self.memory.take(), memory);
                self.plans.push(plan);
                self.pending = None;
            }
        }
        Ok(())
    }

    /// Compile one original statement or resolve one deferred role input. A later statement's analysis error precedes deferred role diagnostics.
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
        if self.pending.is_some() {
            self.advance_pending(control)?;
            return Ok(false);
        }
        if let Some(statement) = statements.get(self.plans.len()) {
            let plan = self
                .compiler
                .compile(context, statement.clone(), self.lowering)?;
            self.record_plan(BoundPlan::new(plan.into(), None), control)?;
            return Ok(false);
        }
        if let Some(roles) = self.roles.get(self.checked_roles) {
            if !roles.validate_next_input(context.regroles, &mut self.next_role_input)? {
                self.checked_roles += 1;
                self.next_role_input = 0;
            }
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
