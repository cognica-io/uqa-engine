//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate captured named builtin sequence arguments without evaluating numeric or dynamic inputs.

use super::routine_dependencies::{self, References};
use crate::catalog::{relation_names::RelationNameCatalog, CatalogReadView};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    ast::{Expr, FunctionBinding, Statement},
    plan::{PlanRef, UnifiedPlan},
    schema::dependencies::rewrites::{
        visit_scalar_sequence_function_references_with_control,
        visit_sequence_function_references_with_control,
    },
    SQLError, ScalarExpr,
};

pub fn validate(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    routine_dependencies::visit(
        catalog.snapshot(),
        &SequenceReferences { catalog, control },
        control,
    )
}

struct SequenceReferences<'a, 'b> {
    catalog: &'a CatalogReadView,
    control: &'a ProductionControl<'b>,
}

impl SequenceReferences<'_, '_> {
    fn reference(&self, reference: &str) -> Result<(), SQLError> {
        let Some(identity) =
            uqa_sql::schema::sequences::native::function_relation(reference, self.control)?
        else {
            return Ok(());
        };
        let snapshot = self.catalog.snapshot();
        let names = RelationNameCatalog::new(&snapshot.tables, &snapshot.definitions);
        // regclass accepts any relation kind; the sequence operation checks its kind at execution.
        if names.kind_at(&identity).is_none()
            && self
                .catalog
                .graph_label_kind(&identity.schema, &identity.name)?
                .is_none()
        {
            return Err(SQLError::Internal(
                "missing native sequence function relation".into(),
            ));
        }
        self.control.check()?;
        Ok(())
    }
}

impl References for SequenceReferences<'_, '_> {
    fn expression(&self, expression: &Expr) -> Result<(), SQLError> {
        visit_sequence_function_references_with_control(expression, self.control, &mut |name| {
            self.reference(name)
        })
    }
    fn scalar(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        visit_scalar_sequence_function_references_with_control(
            expression,
            self.control,
            &mut |name| self.reference(name),
        )
    }
    fn plan(&self, plan: PlanRef<'_>) -> Result<(), SQLError> {
        self.control.check()?;
        plan.try_visit_scalar_expressions(&mut |expression| self.scalar(expression))
    }
    fn statement(&self, statement: &Statement) -> Result<(), SQLError> {
        let plan =
            UnifiedPlan::lower_borrowed_for_rewrite(statement, &|_: &str| false, self.control)?;
        self.plan(plan.as_plan_ref())
    }
    // These carry no argument expression. The independent routine predicate validates their concrete identities.
    fn binding(&self, _: &FunctionBinding) -> Result<(), SQLError> {
        self.control.check()?;
        Ok(())
    }
    fn identity(&self, _: [u8; 16], _: &[String], _: bool) -> Result<(), SQLError> {
        self.control.check()?;
        Ok(())
    }
    fn trigger(&self, _: &str, _: Option<[u8; 16]>) -> Result<(), SQLError> {
        self.control.check()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
