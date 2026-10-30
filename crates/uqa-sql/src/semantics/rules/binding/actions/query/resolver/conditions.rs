//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Condition bindings retain refreshed identities, original scopes and independently owned substitutions.

use super::{insert_expression, OwnedRuleQueryResolver};
use crate::{
    ast::ColumnDef,
    binding::stored_routines::analysis::CatalogRoutineAnalysisContext,
    catalog::{events::StoredRule, roles::RoleReference},
    plan::{
        input_projection::substitute_expression_inputs_owned, ExpressionPlan,
        MutableExecutablePlan, MutableExpressionPlan,
    },
    plpgsql::{RewriteReference, VariableResolver},
    SQLError, ScalarExpr,
};
use uqa_core::memory::{OwnedMap, ProductionControl, ProductionMap};

impl OwnedRuleQueryResolver {
    pub fn condition_with_control(
        &mut self,
        catalog: CatalogRoutineAnalysisContext<'_>,
        rule: &StoredRule,
        columns: &[ColumnDef],
        subject: &RoleReference,
        control: &ProductionControl<'_>,
    ) -> Result<Option<MutableExpressionPlan>, SQLError> {
        control.check()?;
        if rule.definition.condition.is_none() {
            return Ok(None);
        }
        let (original, binding) = rule.bound_condition_plan().ok_or_else(|| {
            SQLError::Internal("stored rule condition has not been analyzed".into())
        })?;
        let (binding, mut condition) = binding.refresh_columns_with_control(
            columns,
            original.copy_for_rewrite(control)?,
            control,
        )?;
        if crate::semantics::rules::action_binding::rule_condition_plan_references_whole_row(
            original,
        ) {
            condition = catalog.bind_rule_inputs_owned(
                condition,
                &binding,
                &self.table,
                columns,
                control,
            )?;
        }
        for index in 0..condition.queries().len() {
            condition = condition.map_query(index, control, |query| {
                let plan =
                    MutableExecutablePlan::Query(query).mark_catalog_relations_bound(control)?;
                let MutableExecutablePlan::Query(query) = plan else {
                    unreachable!()
                };
                query.set_privilege_subject(Some(subject.clone_with_control(control)?), control)
            })?;
        }
        let mut selected = ProductionMap::new(*control);
        let mut inspect = |scalar: &ScalarExpr| {
            scalar.try_visit(&mut |node| {
                control.check()?;
                if let ScalarExpr::InternalColumn(column) = node {
                    if binding.column_name(*column).is_some() {
                        selected.insert_copy(*column, ())?;
                    }
                }
                Ok::<_, SQLError>(true)
            })
        };
        inspect(condition.scalar())?;
        for query in condition.queries() {
            let mut result = Ok(());
            query.visit_scalar_expressions(&mut |scalar| {
                if result.is_ok() {
                    result = inspect(scalar);
                }
            });
            result?;
        }
        let mut replacements = OwnedMap::new();
        for (column, ()) in &selected {
            let side = if Some(column.relation()) == binding.old_relation() {
                "old"
            } else {
                "new"
            };
            let name = binding
                .column_name(*column)
                .expect("condition binding owns its referenced column");
            let expression = self
                .rewrite_with_control(
                    RewriteReference::Qualified {
                        qualifier: side,
                        column: name,
                    },
                    control,
                )?
                .ok_or_else(|| SQLError::UnknownColumn(format!("{side}.{name}")))?;
            let expression = ExpressionPlan::lower_borrowed_for_rewrite(&expression, control)?;
            let expression =
                substitute_expression_inputs_owned(expression, &self.expressions, control)?;
            insert_expression(
                &mut replacements,
                *column,
                expression.finish(control)?,
                control,
            )?;
        }
        Ok(Some(substitute_expression_inputs_owned(
            condition,
            &replacements,
            control,
        )?))
    }
}
