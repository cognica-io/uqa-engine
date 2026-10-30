//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent query inputs for the original INSERT and each view/rule relation.

use super::{
    add_condition, projection_block, BTreeSet, Expr, InternalRelationId, ProjectionPlan, QueryPlan,
    RelationalPlan, RuleQueryResolver, SQLError, ScalarExpr, SourcePlan,
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
        let mut first = relation_plan(insert, insert.view_rule_insert_plans.first());
        let mut values = super::values::ValuesInput::describe(&first)?;
        let mut source = if let Some(values) = &values {
            values.source(context, &mut first)?
        } else {
            insert.source.clone().expect("INSERT query input")
        };
        source
            .relation_privilege_subject
            .get_or_insert(input_subject.clone());
        let mut relations = Vec::new();
        for layer in &insert.view_rule_insert_plans {
            let mut plan = relation_plan(insert, Some(layer));
            plan.source = Some(source);
            if let Some(values) = &mut values {
                values.project_layer(context, &mut plan)?;
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
        let mut plan = relation_plan(insert, None);
        plan.source = Some(source);
        if let Some(values) = &mut values {
            values.project_layer(context, &mut plan)?;
        }
        let original = original_query(context, &plan, subjects)?;
        relations.push(plan);
        let original = if let Some(values) = &values {
            original.map(|query| values.finish(query))
        } else {
            original
        };
        Ok(Self {
            original,
            relations,
        })
    }
}

fn relation_plan(
    insert: &InsertPlan,
    layer: Option<&crate::plan::ViewRuleInsertPlan>,
) -> InsertPlan {
    let mut plan = insert.clone();
    plan.view_rule_insert_plans.clear();
    plan.view_rule_relations.clear();
    let Some(layer) = layer else {
        return plan;
    };
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
    plan
}

fn original_query(
    context: RuleAnalysisContext<'_>,
    insert: &InsertPlan,
    subjects: &dyn Fn(&str) -> Result<crate::catalog::roles::RoleReference, SQLError>,
) -> Result<Option<Box<QueryPlan>>, SQLError> {
    let rules = context
        .rules
        .capture_rules(&insert.table, RuleEvent::Insert)?;
    for rule in rules.iter() {
        let rule = rule?;
        if rule.definition.instead && rule.definition.condition.is_none() {
            return Ok(None);
        }
    }
    let mut conditions = Vec::new();
    for rule in rules.iter() {
        let rule = rule?;
        if rule.definition.instead {
            if let Some(condition) = rule.definition.condition.as_ref() {
                conditions.push(condition);
            }
        }
    }
    if conditions.is_empty() {
        return Ok(insert.source.clone());
    }
    let subject = subjects(&insert.table)?;
    let relation = InternalRelationId::allocate();
    let mut resolver = RuleQueryResolver::from_insert(context, insert, relation)?;
    let source = insert.source.as_deref().expect("INSERT query input");
    let mut block = original_block(insert, source, relation, resolver.columns.len())?;
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

fn original_block(
    insert: &InsertPlan,
    source: &QueryPlan,
    relation: InternalRelationId,
    default_width: usize,
) -> Result<crate::plan::QueryBlockPlan, SQLError> {
    let width = source.analyzed_output.as_ref().map_or_else(
        || match &source.root {
            RelationalPlan::QueryBlock(block) => block.projections.len(),
            RelationalPlan::Values { rows, .. } => rows.first().map_or(0, Vec::len),
            RelationalPlan::SetOp { .. } => {
                if insert.columns.is_empty() {
                    default_width
                } else {
                    insert.columns.len()
                }
            }
        },
        crate::RowSchema::len,
    );
    let inline = insert
        .rule_values_input
        .as_ref()
        .is_some_and(crate::plan::RuleValuesInput::is_inline);
    let mut block = if inline {
        let RelationalPlan::QueryBlock(block) = &source.root else {
            return Err(SQLError::Internal(
                "rule target list lost its query block".into(),
            ));
        };
        (**block).clone()
    } else {
        projection_block(
            (0..width)
                .map(|position| ProjectionPlan {
                    expr: ScalarExpr::InternalColumn(relation.column(position)),
                    alias: None,
                })
                .collect(),
        )
    };
    if !inline {
        block.from = Some(SourcePlan::Subquery {
            captured_output: Some(relation),
            lateral: false,
            body: Box::new(source.clone()),
            alias: None,
            column_aliases: Vec::new(),
        });
    }
    Ok(block)
}
