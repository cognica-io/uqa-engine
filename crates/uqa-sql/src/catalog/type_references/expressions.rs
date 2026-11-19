//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable selected type references in stored syntax and executable metadata.

use super::{invalid, type_metadata, TypeReferences};
use crate::{
    ast::{CompositeRowBinding, Expr, FunctionBinding, FunctionDispatch, UserTypeKind},
    plan::{PlanRef, UnifiedPlan},
    SQLError, ScalarExpr,
};

impl TypeReferences<'_> {
    /// Inspect selected identities without parsing source text, resolving replacement names or evaluating a value.
    pub fn expression(&self, expression: &Expr) -> Result<(), SQLError> {
        crate::schema::dependencies::walk_schema_expr_with_control(
            expression,
            &self.control(),
            &mut |node| match node {
                Expr::Cast { ty, .. }
                | Expr::TypedLiteral { ty, .. }
                | Expr::CompositeRow {
                    binding: CompositeRowBinding { ty, .. },
                    ..
                } => self.stored_name(ty),
                Expr::Func {
                    binding: Some(binding),
                    ..
                } => self.function_types(binding),
                Expr::RetainedLiteral(literal) => self.retained_literal(&literal.0),
                _ => Ok(()),
            },
        )
    }

    pub fn scalar(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        expression.try_visit(&mut |node| {
            self.cancellation.check()?;
            match node {
                ScalarExpr::Cast { ty, .. }
                | ScalarExpr::TypedLiteral { ty, .. }
                | ScalarExpr::CompositeRow {
                    binding: CompositeRowBinding { ty, .. },
                    ..
                } => {
                    self.stored_name(ty)?;
                }
                ScalarExpr::Func {
                    binding: Some(binding),
                    ..
                } => self.function_types(binding)?,
                ScalarExpr::RetainedLiteral(literal) => self.retained_literal(literal)?,
                _ => {}
            }
            match node {
                ScalarExpr::TypedLiteral {
                    bound_type: Some(ty),
                    ..
                }
                | ScalarExpr::CompositeRow {
                    bound_type: Some(ty),
                    ..
                }
                | ScalarExpr::Collated { ty, .. } => self.column_type(ty)?,
                _ => {}
            }
            Ok(true)
        })
    }

    fn retained_literal(&self, literal: &crate::ir::RetainedScalarLiteral) -> Result<(), SQLError> {
        if let Some(ty) = &literal.ty {
            self.column_type(ty)?;
        }
        if let Some(declaration) = &literal.declaration {
            self.stored_name(declaration)?;
        }
        Ok(())
    }

    pub fn function_types(&self, binding: &FunctionBinding) -> Result<(), SQLError> {
        self.cancellation.check()?;
        for ty in &binding.argument_types {
            self.stored_name(ty)?;
        }
        if let Some(invocation) = &binding.invocation {
            for ty in invocation
                .argument_targets
                .iter()
                .chain(invocation.argument_sources.iter().flatten())
                .chain(&invocation.parameter_types)
                .chain(&invocation.return_type)
            {
                self.stored_name(ty)?;
            }
        }
        match binding.dispatch {
            Some(FunctionDispatch::Enum { type_oid, .. }) => {
                self.claim(type_oid, UserTypeKind::Enum)?;
            }
            Some(FunctionDispatch::TypeOf { type_oid })
            | Some(FunctionDispatch::CollationFor {
                noncollatable_type_oid: Some(type_oid),
                ..
            }) => self.type_oid(type_oid)?,
            _ => {}
        }
        Ok(())
    }

    fn type_oid(&self, oid: u32) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if self.claims.contains_key(&oid)
            || type_metadata::builtin_scalar_type(oid).is_some()
            || type_metadata::builtin_array_element(oid).is_some()
            || type_metadata::routine_pseudo_type_name(i64::from(oid)).is_some()
            || super::super::is_system_catalog_type_oid(oid)
        {
            Ok(())
        } else {
            Err(invalid("missing selected expression type"))
        }
    }

    pub fn plan(&self, plan: PlanRef<'_>) -> Result<(), SQLError> {
        self.cancellation.check()?;
        plan.try_visit_scalar_expressions(&mut |root| self.scalar(root))?;
        crate::binding::view_dependencies::visit_stored_plan_function_bindings(
            plan,
            &self.control(),
            &mut |binding| self.function_types(binding),
        )?;
        crate::binding::view_dependencies::visit_stored_plan_output_types(
            plan,
            &self.control(),
            &mut |ty| self.stored_name(ty),
            &mut |ty| self.column_type(ty),
        )
    }

    pub fn statement(&self, statement: &crate::ast::Statement) -> Result<(), SQLError> {
        let control = self.control();
        let plan = UnifiedPlan::lower_borrowed_for_rewrite(statement, &|_: &str| false, &control)?;
        self.plan(plan.as_plan_ref())
    }
}
