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
            if let UnifiedPlan::Query(query) = &mut plan {
                let origin = if lowering.persisted_definition {
                    crate::binding::stored_routines::StoredCallOrigin::Persisted
                } else {
                    crate::binding::stored_routines::StoredCallOrigin::Definition
                };
                origin.bind_query(
                    context.routines,
                    query,
                    &self.positional,
                    &binding.context(),
                    None,
                )?;
            }
        }
        Ok(plan)
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
            let mut plan = self
                .compiler
                .compile(context, statement.clone(), self.lowering)?;
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
