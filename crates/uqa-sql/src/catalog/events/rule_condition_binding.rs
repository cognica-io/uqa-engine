//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural OLD/NEW row bindings for persisted rewrite-rule conditions.

use crate::ast::{InternalColumnRef, InternalRelationId, RuleEvent};
use crate::ir::ScalarExpr;
use crate::plan::ExpressionPlan;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleConditionBinding {
    old_relation: Option<InternalRelationId>,
    new_relation: Option<InternalRelationId>,
    columns: Vec<String>,
}

impl RuleConditionBinding {
    pub fn for_event(columns: &[String], event: RuleEvent) -> Self {
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

    pub const fn old_relation(&self) -> Option<InternalRelationId> {
        self.old_relation
    }

    pub const fn new_relation(&self) -> Option<InternalRelationId> {
        self.new_relation
    }

    pub fn old_column(&self, name: &str) -> Option<InternalColumnRef> {
        self.column(self.old_relation, name)
    }

    pub fn new_column(&self, name: &str) -> Option<InternalColumnRef> {
        self.column(self.new_relation, name)
    }

    pub fn column_name(&self, column: InternalColumnRef) -> Option<&str> {
        if Some(column.relation()) != self.old_relation
            && Some(column.relation()) != self.new_relation
        {
            return None;
        }
        self.columns.get(column.attribute()).map(String::as_str)
    }

    /// Give a deserialized condition plan process-local row identities before it can be combined with newly planned expressions.
    pub fn reallocate_plan_relations(&self, plan: &mut ExpressionPlan) -> Self {
        let rebound = Self {
            old_relation: self.old_relation.map(|_| InternalRelationId::allocate()),
            new_relation: self.new_relation.map(|_| InternalRelationId::allocate()),
            columns: self.columns.clone(),
        };
        self.remap_plan(plan, &rebound);
        rebound
    }

    /// A whole-row reference follows the live event row after ADD/DROP COLUMN, while existing field references retain their names.
    pub fn refresh_columns(&self, columns: Vec<String>, plan: &mut ExpressionPlan) -> Self {
        if self.columns == columns {
            return self.clone();
        }
        let rebound = Self {
            old_relation: self.old_relation.map(|_| InternalRelationId::allocate()),
            new_relation: self.new_relation.map(|_| InternalRelationId::allocate()),
            columns,
        };
        self.remap_plan(plan, &rebound);
        rebound
    }

    /// Form the current event row without evaluating any of its fields.
    pub fn whole_row_expression(&self, qualifier: &str, table: &str) -> Option<ScalarExpr> {
        let relation = match qualifier {
            "old" => self.old_relation?,
            "new" => self.new_relation?,
            _ => return None,
        };
        Some(ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Row(
                (0..self.columns.len())
                    .map(|position| ScalarExpr::InternalColumn(relation.column(position)))
                    .collect(),
            )),
            ty: table.to_string(),
        })
    }

    pub fn referenced_columns(
        &self,
        plan: &ExpressionPlan,
    ) -> std::collections::BTreeSet<InternalColumnRef> {
        let mut required = std::collections::BTreeSet::new();
        let mut collect = |expression: &ScalarExpr| {
            expression.visit(&mut |node| {
                if let ScalarExpr::InternalColumn(column) = node {
                    if self.column_name(*column).is_some() {
                        required.insert(*column);
                    }
                }
            });
        };
        collect(&plan.scalar);
        for query in &plan.subqueries {
            query.visit_scalar_expressions(&mut collect);
        }
        required
    }

    pub fn row_schema(&self, columns: &[(String, crate::ast::ColumnType)]) -> crate::RowSchema {
        let mut names = Vec::with_capacity(columns.len() * 2);
        let mut identities = Vec::with_capacity(columns.len() * 2);
        let mut types = Vec::with_capacity(columns.len() * 2);
        let mut internal = Vec::with_capacity(columns.len() * 2);
        for (side, relation) in [("old", self.old_relation()), ("new", self.new_relation())] {
            let Some(relation) = relation else {
                continue;
            };
            for (attribute, (name, ty)) in columns.iter().enumerate() {
                let slot = names.len();
                names.push(name.clone());
                identities.push(crate::ColumnIdentity::qualified(side, name));
                types.push(Some(ty.clone()));
                internal.push((relation.column(attribute), slot, Some(ty.clone())));
            }
        }
        let schema = crate::RowSchema::with_identities(names, identities, types);
        crate::RowSchema::with_physical_internal_aliases(&schema, &internal)
    }

    fn remap_plan(&self, plan: &mut ExpressionPlan, rebound: &Self) {
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
                if let Some(name) = self.column_name(*column) {
                    if let Some(replacement) = rebound.column(Some(relation), name) {
                        *column = replacement;
                    }
                }
            }
        };
        crate::plan::rewrite_scalar_expression(&mut plan.scalar, &mut rewrite);
        for subquery in &mut plan.subqueries {
            subquery.rewrite_scalar_expressions(&mut rewrite);
        }
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
    fn refreshed_columns_keep_field_names_and_expand_the_live_whole_row() {
        let binding = RuleConditionBinding::for_event(
            &["id".into(), "value".into(), "unused".into()],
            RuleEvent::Update,
        );
        let mut plan = ExpressionPlan {
            scalar: ScalarExpr::Row(vec![
                ScalarExpr::InternalColumn(binding.new_column("value").unwrap()),
                ScalarExpr::Column("old".into()),
            ]),
            subqueries: Vec::new(),
        };
        let refreshed =
            binding.refresh_columns(vec!["value".into(), "id".into(), "added".into()], &mut plan);
        assert_eq!(
            refreshed.referenced_columns(&plan),
            std::collections::BTreeSet::from([refreshed.new_column("value").unwrap()])
        );
        let ScalarExpr::Row(items) = &plan.scalar else {
            panic!("outer row");
        };
        assert_eq!(
            items[0],
            ScalarExpr::InternalColumn(refreshed.new_column("value").unwrap())
        );
        let whole = refreshed
            .whole_row_expression("old", "public.event")
            .unwrap();
        let ScalarExpr::Cast { expr, ty, .. } = &whole else {
            panic!("typed whole row");
        };
        assert_eq!(ty, "public.event");
        assert_eq!(
            expr.as_ref(),
            &ScalarExpr::Row(
                ["value", "id", "added"]
                    .map(|name| ScalarExpr::InternalColumn(refreshed.old_column(name).unwrap()))
                    .to_vec()
            )
        );
    }
}
