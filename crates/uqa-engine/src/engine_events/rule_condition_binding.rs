//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural OLD/NEW row bindings for persisted rewrite-rule conditions.

use serde::{Deserialize, Serialize};
use uqa_execution::ScalarExpr;
use uqa_planner::ExpressionPlan;
use uqa_sql::ast::{InternalColumnRef, InternalRelationId, RuleEvent};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RuleConditionBinding {
    old_relation: Option<InternalRelationId>,
    new_relation: Option<InternalRelationId>,
    columns: Vec<String>,
}

impl RuleConditionBinding {
    pub(crate) fn for_event(columns: &[String], event: RuleEvent) -> Self {
        let old_relation = matches!(event, RuleEvent::Update | RuleEvent::Delete)
            .then(InternalRelationId::allocate);
        let new_relation = matches!(event, RuleEvent::Insert | RuleEvent::Update)
            .then(InternalRelationId::allocate);
        Self {
            old_relation,
            new_relation,
            columns: columns.to_vec(),
        }
    }

    pub(crate) const fn old_relation(&self) -> Option<InternalRelationId> {
        self.old_relation
    }

    pub(crate) const fn new_relation(&self) -> Option<InternalRelationId> {
        self.new_relation
    }

    pub(crate) fn old_column(&self, name: &str) -> Option<InternalColumnRef> {
        self.column(self.old_relation, name)
    }

    pub(crate) fn new_column(&self, name: &str) -> Option<InternalColumnRef> {
        self.column(self.new_relation, name)
    }

    pub(crate) fn column_name(&self, column: InternalColumnRef) -> Option<&str> {
        if Some(column.relation()) != self.old_relation
            && Some(column.relation()) != self.new_relation
        {
            return None;
        }
        self.columns.get(column.attribute()).map(String::as_str)
    }

    /// Give a deserialized condition plan process-local row identities before it can be combined with newly planned expressions.
    pub(crate) fn reallocate_plan_relations(&self, plan: &mut ExpressionPlan) -> Self {
        let rebound = Self {
            old_relation: self.old_relation.map(|_| InternalRelationId::allocate()),
            new_relation: self.new_relation.map(|_| InternalRelationId::allocate()),
            columns: self.columns.clone(),
        };
        let mut rewrite = |expression: &mut ScalarExpr| {
            let ScalarExpr::InternalColumn(column) = expression else {
                return;
            };
            let relation = if Some(column.relation()) == self.old_relation {
                rebound.old_relation
            } else if Some(column.relation()) == self.new_relation {
                rebound.new_relation
            } else {
                None
            };
            if let Some(relation) = relation {
                *column = relation.column(column.attribute());
            }
        };
        uqa_planner::rewrite_scalar_expression(&mut plan.scalar, &mut rewrite);
        for subquery in &mut plan.subqueries {
            subquery.rewrite_scalar_expressions(&mut rewrite);
        }
        rebound
    }

    fn column(
        &self,
        relation: Option<InternalRelationId>,
        name: &str,
    ) -> Option<InternalColumnRef> {
        let relation = relation?;
        self.columns
            .iter()
            .position(|column| column == name)
            .map(|position| relation.column(position))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialized_plan_relations_are_reallocated_and_rewritten_together() {
        let binding = RuleConditionBinding {
            old_relation: Some(InternalRelationId::from_raw(u64::MAX - 1)),
            new_relation: Some(InternalRelationId::from_raw(u64::MAX)),
            columns: vec!["id".into()],
        };
        let mut plan = ExpressionPlan {
            scalar: ScalarExpr::InternalColumn(binding.old_column("id").unwrap()),
            subqueries: Vec::new(),
        };

        let rebound = binding.reallocate_plan_relations(&mut plan);
        let ScalarExpr::InternalColumn(column) = plan.scalar else {
            panic!("condition plan lost its structural OLD column")
        };
        assert_eq!(Some(column.relation()), rebound.old_relation());
        assert_eq!(rebound.column_name(column), Some("id"));
        assert_ne!(rebound.old_relation(), binding.old_relation());
        assert_ne!(rebound.new_relation(), binding.new_relation());
    }

    #[test]
    fn stored_rule_conditions_use_only_structural_event_row_references() {
        let engine = crate::Engine::new();
        engine
            .sql(
                "CREATE TABLE structural_rule_items(id integer);
                 CREATE RULE structural_rule_condition AS ON INSERT TO structural_rule_items
                   WHERE EXISTS (SELECT 1 WHERE NEW.id > 0) DO NOTHING",
                &[],
            )
            .unwrap();
        let rule = engine
            .rules_for("public.structural_rule_items", RuleEvent::Insert)
            .unwrap()
            .pop()
            .unwrap();
        let (plan, binding) = rule.bound_condition_plan().unwrap();
        let expected = binding.new_column("id").unwrap();
        let mut internal_columns = Vec::new();
        let mut contains_nul_qualifier = false;
        let mut inspect = |expression: &mut ScalarExpr| match expression {
            ScalarExpr::InternalColumn(column) => internal_columns.push(*column),
            ScalarExpr::QualifiedColumn { qualifier, .. } => {
                contains_nul_qualifier |= qualifier.contains('\0');
            }
            _ => {}
        };
        let mut scalar = plan.scalar.clone();
        uqa_planner::rewrite_scalar_expression(&mut scalar, &mut inspect);
        for subquery in &plan.subqueries {
            let mut subquery = subquery.clone();
            subquery.rewrite_scalar_expressions(&mut inspect);
        }
        assert!(internal_columns.contains(&expected));
        assert!(!contains_nul_qualifier);

        let serialized = serde_json::to_string(&rule).unwrap();
        assert!(!serialized.contains("\\u0000"));
        assert!(!serialized.contains('\0'));
    }
}
