//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable selected type references in stored syntax and executable metadata.

use super::{inline, invalid, type_metadata, TypeReferences, Work};
use crate::{
    ast::{CompositeRowBinding, Expr, FunctionBinding, FunctionDispatch, UserTypeKind},
    binding::view_dependencies::{visit_plan_type_metadata, StoredPlanOutput, TypeObserver},
    ir::observation::{visit_all, ScalarObserver},
    plan::{PlanRef, UnifiedPlan},
    SQLError, ScalarExpr,
};

impl TypeReferences<'_> {
    /// Inspect selected identities without parsing source text, resolving replacement names or evaluating a value.
    pub fn expression(&self, expression: &Expr) -> Result<(), SQLError> {
        inline(self.expression_with_work(expression, &mut ()))
    }

    pub async fn expression_with_work(
        &self,
        expression: &Expr,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        let control = self.control();
        let mut nodes = expression.stored_nodes(self.memory)?;
        while let Some(node) = nodes.next(&control, work).await? {
            match node {
                Expr::Cast { ty, .. }
                | Expr::TypedLiteral { ty, .. }
                | Expr::CompositeRow {
                    binding: CompositeRowBinding { ty, .. },
                    ..
                } => self.stored_name_with_work(ty, work).await?,
                Expr::Func {
                    binding: Some(binding),
                    ..
                } => self.function_types_with_work(binding, work).await?,
                Expr::RetainedLiteral(literal) => {
                    self.retained_literal_with_work(&literal.0, work).await?;
                }
                Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
                    return Err(crate::schema::dependencies::subquery_error(&control));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn scalar(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        let mut observer = self;
        inline(visit_all(
            expression,
            &self.control(),
            &mut (),
            &mut observer,
        ))
    }

    async fn retained_literal_with_work(
        &self,
        literal: &crate::ir::RetainedScalarLiteral,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        if let Some(ty) = &literal.ty {
            self.column_type_with_work(ty, work).await?;
        }
        if let Some(declaration) = &literal.declaration {
            self.stored_name_with_work(declaration, work).await?;
        }
        Ok(())
    }

    pub fn function_types(&self, binding: &FunctionBinding) -> Result<(), SQLError> {
        inline(self.function_types_with_work(binding, &mut ()))
    }

    async fn function_types_with_work(
        &self,
        binding: &FunctionBinding,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        self.cancellation.check()?;
        for ty in &binding.argument_types {
            self.stored_name_with_work(ty, work).await?;
        }
        if let Some(invocation) = &binding.invocation {
            for ty in &invocation.argument_targets {
                self.stored_name_with_work(ty, work).await?;
            }
            for source in &invocation.argument_sources {
                if let Some(ty) = source {
                    self.stored_name_with_work(ty, work).await?;
                } else {
                    self.checkpoint(work).await?;
                }
            }
            for ty in invocation
                .parameter_types
                .iter()
                .chain(&invocation.return_type)
            {
                self.stored_name_with_work(ty, work).await?;
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
        inline(self.plan_with_work(plan, &mut ()))
    }

    pub async fn plan_with_work(
        &self,
        plan: PlanRef<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        let control = self.control();
        let mut observer = self;
        crate::plan::visit_scalar_nodes_with_work(plan, &control, work, &mut observer).await?;
        visit_plan_type_metadata(plan, &control, work, &mut observer).await
    }

    pub fn statement(&self, statement: &crate::ast::Statement) -> Result<(), SQLError> {
        inline(self.statement_with_work(statement, &mut ()))
    }

    pub async fn statement_with_work(
        &self,
        statement: &crate::ast::Statement,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        let control = self.control();
        let plan = UnifiedPlan::lower_borrowed_for_rewrite(statement, &|_: &str| false, &control)?;
        self.plan_with_work(plan.as_plan_ref(), work).await
    }
}

impl ScalarObserver for &TypeReferences<'_> {
    async fn scalar<W: Work + Send>(
        &mut self,
        node: &ScalarExpr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        match node {
            ScalarExpr::Cast { ty, .. }
            | ScalarExpr::TypedLiteral { ty, .. }
            | ScalarExpr::CompositeRow {
                binding: CompositeRowBinding { ty, .. },
                ..
            } => {
                self.stored_name_with_work(ty, work).await?;
            }
            ScalarExpr::Func {
                binding: Some(binding),
                ..
            } => self.function_types_with_work(binding, work).await?,
            ScalarExpr::RetainedLiteral(literal) => {
                self.retained_literal_with_work(literal, work).await?;
            }
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
            | ScalarExpr::Collated { ty, .. } => self.column_type_with_work(ty, work).await?,
            _ => {}
        }
        Ok(())
    }
}

impl TypeObserver for &TypeReferences<'_> {
    async fn binding<W: Work + Send>(
        &mut self,
        binding: &FunctionBinding,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.function_types_with_work(binding, work).await
    }

    async fn output<W: Work + Send>(
        &mut self,
        value: StoredPlanOutput<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        match value {
            StoredPlanOutput::Name(name) => self.stored_name_with_work(name, work).await,
            StoredPlanOutput::Concrete(ty) => self.column_type_with_work(ty, work).await,
        }
    }
}
