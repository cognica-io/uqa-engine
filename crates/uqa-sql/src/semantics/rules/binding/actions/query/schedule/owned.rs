//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each INSERT relation retains an independent admitted query until its original or rule action consumes it.

use super::super::{values::ValuesInput, OwnedRuleQueryResolver};
use crate::{
    ast::{BooleanTest, FunctionBinding, FunctionDispatch, InternalRelationId, RuleEvent},
    catalog::roles::RoleReference,
    plan::{
        input_projection::substitute_expression_inputs_owned, CommandPlan, ExpressionPlan,
        InsertPlan, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan, MutableSourcePlan,
        OwnedProjection, OwnedScalarExpr, QueryPlan, RelationalPlan,
    },
    production::Buffer,
    semantics::rules::{
        action_binding::bind_rule_expr_scoped_with_control, analysis::RuleAnalysisContext,
    },
    SQLError, ScalarExpr,
};
use std::collections::BTreeSet;
use uqa_core::memory::{Produced, ProductionControl};

pub struct OwnedInsertRuleQueries {
    pub original: Option<MutableQueryPlan>,
    relations: Buffer<MutableCommandPlan>,
}

impl OwnedInsertRuleQueries {
    pub fn relations(&self) -> impl DoubleEndedIterator<Item = &InsertPlan> {
        self.relations.iter().map(insert_plan)
    }

    pub fn reserved_bytes(&self) -> usize {
        self.original
            .as_ref()
            .map_or(0, MutableQueryPlan::reserved_bytes)
            + self
                .relations
                .iter()
                .map(MutableCommandPlan::reserved_bytes)
                .sum::<usize>()
            + self.relations.reserved_bytes()
    }

    pub fn bind(
        context: RuleAnalysisContext<'_>,
        insert: &InsertPlan,
        subjects: &dyn Fn(&str) -> Result<RoleReference, SQLError>,
        input_subject: &RoleReference,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (mut values, mut source) = initial_source(context, insert, control)?;
        if source.relation_privilege_subject.is_none() {
            source = source
                .set_privilege_subject(Some(input_subject.clone_with_control(control)?), control)?;
        }
        let mut relations = Buffer::new(control);
        for layer in insert
            .view_rule_insert_plans
            .iter()
            .map(Some)
            .chain(std::iter::once(None))
        {
            let command = insert.copy_rule_layer_for_rewrite(layer, false, control)?;
            let mut command = command.map_insert_source(control, |_, previous| {
                debug_assert!(previous.is_none());
                Ok(source)
            })?;
            if let Some(input) = &values {
                command = input.project_layer_with_control(context, command, control)?;
                values = ValuesInput::describe_with_control(insert_plan(&command), control)?;
            }
            let original = original_query(context, insert_plan(&command), subjects, control)?;
            relations.push(command, control)?;
            let Some(original) = original else {
                return Ok(Self {
                    original: None,
                    relations,
                });
            };
            source = original;
        }
        let source = if let Some(values) = values {
            finish_values(&values, source, control)?
        } else {
            source
        };
        Ok(Self {
            original: Some(source),
            relations,
        })
    }
}

fn insert_plan(command: &MutableCommandPlan) -> &InsertPlan {
    let CommandPlan::Insert(insert) = &**command else {
        unreachable!("INSERT schedule owner")
    };
    insert
}

fn initial_source(
    context: RuleAnalysisContext<'_>,
    insert: &InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<(Option<Produced<ValuesInput>>, MutableQueryPlan), SQLError> {
    control.check()?;
    if let Some(source) = &insert.source {
        return Ok((
            ValuesInput::describe_with_control(insert, control)?,
            source.copy_for_rewrite(control)?,
        ));
    }
    let command =
        insert.copy_rule_layer_for_rewrite(insert.view_rule_insert_plans.first(), true, control)?;
    let values =
        ValuesInput::describe_with_control(insert_plan(&command), control)?.expect("VALUES input");
    let source = values.initial_source_with_control(context, command, control)?;
    Ok((Some(values), source))
}

fn original_query(
    context: RuleAnalysisContext<'_>,
    insert: &InsertPlan,
    subjects: &dyn Fn(&str) -> Result<RoleReference, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<Option<MutableQueryPlan>, SQLError> {
    let rules =
        context
            .rules
            .capture_rules_with_control(&insert.table, RuleEvent::Insert, control)?;
    for rule in rules.iter() {
        control.check()?;
        let rule = rule?;
        if rule.definition.instead && rule.definition.condition.is_none() {
            return Ok(None);
        }
    }
    let mut conditions = Buffer::new(control);
    for rule in rules.iter() {
        control.check()?;
        let rule = rule?;
        if rule.definition.instead {
            if let Some(condition) = &rule.definition.condition {
                conditions.push(condition, control)?;
            }
        }
    }
    let source = insert.source.as_deref().expect("INSERT query input");
    if conditions.is_empty() {
        return Ok(Some(source.copy_for_rewrite(control)?));
    }
    let subject = subjects(&insert.table)?;
    let relation = InternalRelationId::allocate();
    let mut resolver =
        OwnedRuleQueryResolver::from_insert_with_control(context, insert, relation, control)?;
    let mut query = original_block(insert, source, relation, resolver.column_count(), control)?;
    for condition in conditions {
        let expression = bind_rule_expr_scoped_with_control(
            condition,
            &mut resolver,
            &BTreeSet::new(),
            control,
        )?;
        let mut condition = ExpressionPlan::lower_borrowed_for_rewrite(&expression, control)?;
        condition = condition.map_scalar(control, |scalar, _| {
            let binding = FunctionBinding::dispatched_with_control(
                FunctionDispatch::BooleanTest(BooleanTest::IsNotTrue),
                control,
            )?;
            Ok(OwnedScalarExpr::call(
                control.copy_text(&binding.name)?,
                Some(binding),
                std::iter::once(scalar),
                control,
            )?)
        })?;
        for index in 0..condition.queries().len() {
            condition = condition.map_query(index, control, |query| {
                let MutableExecutablePlan::Query(query) =
                    MutableExecutablePlan::Query(query).mark_catalog_relations_bound(control)?
                else {
                    unreachable!()
                };
                query.set_privilege_subject(Some(subject.clone_with_control(control)?), control)
            })?;
        }
        query = query.map_where_input(control, |previous, arena| {
            let condition = arena.append_expression(condition, control)?;
            Ok(Some(match previous {
                Some(previous) => {
                    OwnedScalarExpr::boolean([previous, condition].into_iter(), true, control)?
                }
                None => condition,
            }))
        })?;
    }
    let (query, expression) = query.take_where_expression(control)?;
    let query = query.replace_where_expression(
        substitute_expression_inputs_owned(
            expression.expect("original rule condition"),
            &resolver.expressions,
            control,
        )?,
        control,
    )?;
    Ok(Some(
        query
            .set_analyzed_output(source.analyzed_output.clone(), control)?
            .set_privilege_subject(
                source
                    .relation_privilege_subject
                    .as_ref()
                    .map(|subject| subject.clone_with_control(control))
                    .transpose()?,
                control,
            )?
            .set_relations_bound(insert.relations_bound, control)?,
    ))
}

fn original_block(
    insert: &InsertPlan,
    source: &QueryPlan,
    relation: InternalRelationId,
    default_width: usize,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    if insert
        .rule_values_input
        .as_ref()
        .is_some_and(ValuesInput::is_inline)
    {
        let RelationalPlan::QueryBlock(block) = &source.root else {
            return Err(SQLError::Internal(
                "rule target list lost its query block".into(),
            ));
        };
        return block.copy_for_rewrite(control);
    }
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
    captured_projection(source.copy_for_rewrite(control)?, relation, width, control)
}

fn captured_projection(
    source: MutableQueryPlan,
    relation: InternalRelationId,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let mut projections = Buffer::new(control);
    for position in 0..width {
        projections.push(
            OwnedProjection {
                expression: ScalarExpr::InternalColumn(relation.column(position))
                    .copy_for_rewrite(control)?,
                alias: None,
            },
            control,
        )?;
    }
    let query = MutableQueryPlan::projected(projections.into_iter(), control)?;
    let source = MutableSourcePlan::subquery(
        source,
        None,
        control.finish(Vec::new(), control.empty_reservation())?,
        Some(relation),
        false,
        control,
    )?;
    query.replace_source(Some(source), control)
}

fn finish_values(
    values: &ValuesInput,
    source: MutableQueryPlan,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    if values.is_inline() || values.defaults.is_empty() {
        return Ok(source);
    }
    let subject = source
        .relation_privilege_subject
        .as_ref()
        .map(|subject| subject.clone_with_control(control))
        .transpose()?;
    let bound = source.relations_bound;
    let source = source.set_analyzed_output(None, control)?;
    captured_projection(
        source,
        InternalRelationId::allocate(),
        values.width,
        control,
    )?
    .set_privilege_subject(subject, control)?
    .set_relations_bound(bound, control)
}
