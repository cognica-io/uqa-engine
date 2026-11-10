//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concrete routine references retain their original identities without resolving new overloads.

use crate::{
    ast::{CreateFunction, Expr, FunctionBinding, Statement},
    plan::PlanRef,
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{BudgetedMap, MemoryBudget, ProductionControl},
    CancellationToken,
};

/// An admitted borrowed identity index for one immutable routine registry. Late-bound calls and deferred source bodies retain their existing execution contract; runtime resource availability is checked separately.
pub struct NativeRoutineReferences<'a> {
    definitions: BudgetedMap<[u8; 16], &'a CreateFunction>,
    control: ProductionControl<'a>,
}

impl<'a> NativeRoutineReferences<'a> {
    pub fn new(
        definitions: impl Iterator<Item = &'a CreateFunction>,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, SQLError> {
        let control = ProductionControl::new(memory, cancellation, cancellation);
        control.check()?;
        let mut registry = BudgetedMap::new(memory);
        for definition in definitions {
            control.check()?;
            let identity = definition
                .object_id
                .ok_or_else(|| invalid("missing identity"))?;
            if identity == [0; 16] || registry.insert(identity, definition)?.is_some() {
                return Err(invalid("duplicate or zero identity"));
            }
        }
        Ok(Self {
            definitions: registry,
            control,
        })
    }

    /// Validate a selected function identity and its original declared signature. Display names can be historical after a rename; the ordinary lookup contract selects a concrete identity independently of that spelling.
    pub fn binding(&self, binding: &FunctionBinding) -> Result<(), SQLError> {
        self.control.check()?;
        if binding.builtin || binding.dispatch.is_some() || binding.resolution_error.is_some() {
            return Ok(());
        }
        if let Some(identity) = binding.object_id {
            self.identity(identity, &binding.argument_types, false)?;
        }
        Ok(())
    }

    pub fn identity(
        &self,
        identity: [u8; 16],
        argument_types: &[String],
        procedure: bool,
    ) -> Result<(), SQLError> {
        self.definition(identity, argument_types, procedure)
            .map(|_| ())
    }

    /// A stored trigger selects a zero-argument function with the ordinary trigger return and language contract. Its deferred body remains untouched.
    pub fn trigger(&self, identity: [u8; 16]) -> Result<(), SQLError> {
        crate::catalog::events::validation::validate_trigger_function(self.definition(
            identity,
            &[],
            false,
        )?)
    }

    fn definition(
        &self,
        identity: [u8; 16],
        argument_types: &[String],
        procedure: bool,
    ) -> Result<&CreateFunction, SQLError> {
        self.control.check()?;
        let definition = self
            .definitions
            .get(&identity)
            .ok_or_else(|| invalid("missing bound identity"))?;
        if definition.is_procedure != procedure
            || !super::routine_signature_matches_with_control(
                definition,
                argument_types,
                &self.control,
            )?
        {
            return Err(invalid("bound signature or kind"));
        }
        Ok(definition)
    }

    pub fn expression(&self, expression: &Expr) -> Result<(), SQLError> {
        crate::schema::dependencies::walk_schema_expr_with_control(
            expression,
            &self.control,
            &mut |node| {
                if let Expr::Func {
                    binding: Some(binding),
                    ..
                } = node
                {
                    self.binding(binding)?;
                }
                Ok(())
            },
        )
    }

    pub fn scalar(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        expression.try_visit(&mut |node| {
            self.control.check()?;
            if let ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = node
            {
                self.binding(binding)?;
            }
            Ok(true)
        })
    }

    pub fn plan(&self, plan: PlanRef<'_>) -> Result<(), SQLError> {
        self.control.check()?;
        plan.try_visit_scalar_expressions(&mut |expression| self.scalar(expression))?;
        crate::binding::view_dependencies::visit_stored_plan_function_bindings(
            plan,
            &self.control,
            &mut |binding| self.binding(binding),
        )
    }

    /// Stored SQL actions use the existing admitted syntax lowering, preserving concrete bindings without resolving overloads or evaluating expressions. Deferred source text is never passed here.
    pub fn statement(&self, statement: &Statement) -> Result<(), SQLError> {
        self.control.check()?;
        let plan = crate::plan::UnifiedPlan::lower_borrowed_for_rewrite(
            statement,
            &|_: &str| false,
            &self.control,
        )?;
        self.plan(plan.as_plan_ref())
    }
}

fn invalid(detail: &str) -> SQLError {
    SQLError::Internal(format!("invalid native routine reference: {detail}"))
}

#[cfg(test)]
mod tests;
