//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent query inputs for the original INSERT and each view/rule relation.

use super::{
    add_condition, projection_block, BTreeSet, Expr, InsertQueryResolver, InternalRelationId,
    ProjectionPlan, QueryPlan, RelationalPlan, SQLError, ScalarExpr, SourcePlan,
};
use crate::{
    ast::{BooleanTest, FunctionBinding, FunctionDispatch, FunctionOrderSyntax, RuleEvent},
    plan::InsertPlan,
    semantics::rules::{action_binding::bind_rule_expr_scoped, analysis::RuleAnalysisContext},
};

pub struct InsertRuleQueries {
    pub original: Option<Box<QueryPlan>>,
    /// Outermost view first; INSERT executes these actions in reverse order after its original query.
    pub relations: Vec<InsertPlan>,
}

impl InsertRuleQueries {
    pub fn bind(
        context: RuleAnalysisContext<'_>,
        insert: &InsertPlan,
        subjects: &dyn Fn(&str) -> Result<crate::catalog::roles::RoleReference, SQLError>,
        input_subject: &crate::catalog::roles::RoleReference,
    ) -> Result<Self, SQLError> {
        let mut source = insert.source.clone().expect("INSERT query input");
        source
            .relation_privilege_subject
            .get_or_insert(input_subject.clone());
        let mut relations = Vec::new();
        for layer in &insert.view_rule_insert_plans {
            let mut plan = insert.clone();
            plan.table.clone_from(&layer.relation);
            plan.target_qualifier.clone_from(&layer.relation);
            plan.columns = layer
                .supplied_columns
                .iter()
                .zip(&insert.columns)
                .map(|(name, target)| {
                    let mut target = target.clone();
                    target.column.clone_from(name);
                    target
                })
                .collect();
            plan.source = Some(source);
            plan.view_rule_insert_plans.clear();
            plan.view_rule_relations.clear();
            plan.returning.clear();
            if let Some(returning) = &insert.view_rule_returning {
                if returning.relation == layer.relation {
                    plan.target_qualifier
                        .clone_from(&returning.target_qualifier);
                    plan.returning.clone_from(&returning.returning);
                    plan.returning_aliases.clone_from(&returning.aliases);
                    plan.subqueries.clone_from(&returning.subqueries);
                }
            }
            let original = original_query(context, &plan, subjects)?;
            relations.push(plan);
            let Some(original) = original else {
                return Ok(Self {
                    original: None,
                    relations,
                });
            };
            source = original;
        }
        let mut plan = insert.clone();
        plan.source = Some(source);
        plan.view_rule_insert_plans.clear();
        plan.view_rule_relations.clear();
        let original = original_query(context, &plan, subjects)?;
        relations.push(plan);
        Ok(Self {
            original,
            relations,
        })
    }
}

fn original_query(
    context: RuleAnalysisContext<'_>,
    insert: &InsertPlan,
    subjects: &dyn Fn(&str) -> Result<crate::catalog::roles::RoleReference, SQLError>,
) -> Result<Option<Box<QueryPlan>>, SQLError> {
    let rules = context.rules.rules_for(&insert.table, RuleEvent::Insert)?;
    if rules
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none())
    {
        return Ok(None);
    }
    let conditions = rules
        .iter()
        .filter(|rule| rule.definition.instead)
        .filter_map(|rule| rule.definition.condition.as_ref())
        .collect::<Vec<_>>();
    if conditions.is_empty() {
        return Ok(insert.source.clone());
    }
    let subject = subjects(&insert.table)?;
    let relation = InternalRelationId::allocate();
    let mut resolver = InsertQueryResolver::new(context, insert, relation)?;
    let source = insert.source.as_deref().expect("INSERT query input");
    let width = source.analyzed_output.as_ref().map_or_else(
        || {
            if insert.columns.is_empty() {
                resolver.columns.len()
            } else {
                insert.columns.len()
            }
        },
        crate::RowSchema::len,
    );
    let mut block = projection_block(
        (0..width)
            .map(|position| ProjectionPlan {
                expr: ScalarExpr::InternalColumn(relation.column(position)),
                alias: None,
            })
            .collect(),
    );
    block.from = Some(SourcePlan::Subquery {
        captured_output: Some(relation),
        lateral: false,
        body: Box::new(source.clone()),
        alias: None,
        column_aliases: Vec::new(),
    });
    for condition in conditions {
        let expression = bind_rule_expr_scoped(condition, &mut resolver, &BTreeSet::new())?;
        let binding =
            FunctionBinding::dispatched(FunctionDispatch::BooleanTest(BooleanTest::IsNotTrue));
        add_condition(
            &mut block.r#where,
            &mut block.subqueries,
            Some(Expr::Func {
                order_syntax: FunctionOrderSyntax::Ordinary,
                name: binding.name.clone(),
                binding: Some(binding),
                args: vec![expression],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            }),
            Some(&subject),
        );
    }
    if let Some(predicate) = block.r#where.take() {
        let mut expression = crate::plan::ExpressionPlan {
            scalar: predicate,
            subqueries: std::mem::take(&mut block.subqueries),
        };
        crate::plan::input_projection::substitute_expression_inputs(
            &mut expression,
            &resolver.expressions,
        )?;
        block.r#where = Some(expression.scalar);
        block.subqueries = expression.subqueries;
    }
    Ok(Some(Box::new(QueryPlan {
        analyzed_output: source.analyzed_output.clone(),
        analyzed_source: None,
        analyzed_grouping: None,
        relation_privilege_subject: source.relation_privilege_subject.clone(),
        relations_bound: insert.relations_bound,
        ctes: Vec::new(),
        root: RelationalPlan::QueryBlock(Box::new(block)),
    })))
}
