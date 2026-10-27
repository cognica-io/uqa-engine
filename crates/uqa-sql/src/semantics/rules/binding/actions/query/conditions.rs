//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Substitute native OLD/NEW expressions into the stored, analyzed condition.
use super::{BTreeMap, RuleQueryResolver, SQLError, VariableResolver};
use crate::{
    ast::ColumnDef,
    binding::stored_routines::analysis::CatalogRoutineAnalysisContext,
    catalog::{events::StoredRule, roles::RoleReference},
    plan::ExpressionPlan,
};
impl RuleQueryResolver {
    pub fn condition(
        &mut self,
        catalog: CatalogRoutineAnalysisContext<'_>,
        rule: &StoredRule,
        columns: &[ColumnDef],
        subject: &RoleReference,
    ) -> Result<Option<ExpressionPlan>, SQLError> {
        if rule.definition.condition.is_none() {
            return Ok(None);
        }
        let (condition, binding) = rule.bound_condition_plan().ok_or_else(|| {
            SQLError::Internal("stored rule condition has not been analyzed".into())
        })?;
        let mut condition = condition.clone();
        let binding = binding.refresh_columns(
            columns.iter().map(|column| column.name.clone()).collect(),
            &mut condition,
        );
        if crate::semantics::rules::action_binding::rule_condition_plan_references_whole_row(
            &condition,
        ) {
            catalog.bind_rule_inputs(&mut condition, &binding, &self.table, columns)?;
        }
        for query in &mut condition.subqueries {
            crate::binding::stored_routines::mark_query_relations_bound(query);
            query.relation_privilege_subject = Some(subject.clone());
        }
        let mut replacements = BTreeMap::new();
        for column in binding.referenced_columns(&condition) {
            let side = if Some(column.relation()) == binding.old_relation() {
                "old"
            } else {
                "new"
            };
            let name = binding
                .column_name(column)
                .expect("condition binding owns its referenced column");
            let expression = self
                .rewrite_qualified(side, name)?
                .ok_or_else(|| SQLError::UnknownColumn(format!("{side}.{name}")))?;
            let mut expression = ExpressionPlan::lower(expression);
            crate::plan::input_projection::substitute_expression_inputs(
                &mut expression,
                &self.expressions,
            )?;
            replacements.insert(column, expression);
        }
        crate::plan::input_projection::substitute_expression_inputs(&mut condition, &replacements)?;
        Ok(Some(condition))
    }
}
