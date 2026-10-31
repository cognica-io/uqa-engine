//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native rule actions move input trees and scalar namespaces without detaching their allocation owners.

use super::{values::ValuesInput, RuleAction};
use crate::{
    ast::InternalRelationId,
    plan::{
        input_projection::{substitute_plan_inputs_owned, OwnedInputProjections},
        AggregateClassifier, CommandPlan, MutableExecutablePlan, MutableExpressionPlan,
        MutableQueryPlan, MutableSourcePlan, OwnedScalarExpr, QueryChildSlot, RelationalPlan,
        ScalarQueryArena, UnifiedPlan,
    },
    semantics::rules::{action_binding::BoundRuleStatement, analysis::RuleAnalysisContext},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub struct OwnedRuleQueryInput {
    pub query: MutableQueryPlan,
    pub inline: bool,
    pub relation: InternalRelationId,
    pub condition: Option<MutableExpressionPlan>,
    pub expressions: Produced<OwnedInputProjections>,
}

impl RuleAction {
    /// Lower an already bound statement and attach independently owned event inputs under their original request allowance.
    pub fn bind_with_control(
        context: RuleAnalysisContext<'_>,
        aggregates: &dyn AggregateClassifier,
        statement: &BoundRuleStatement,
        input: Option<OwnedRuleQueryInput>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let plan =
            UnifiedPlan::lower_borrowed_for_rewrite(statement.statement(), aggregates, control)?
                .mark_catalog_relations_bound(control)?;
        let plan = match input {
            Some(input) => input.attach(plan, context, control)?,
            None => plan,
        };
        Ok(Self::Owned(plan))
    }
}

impl OwnedRuleQueryInput {
    pub fn attach(
        self,
        plan: MutableExecutablePlan,
        context: RuleAnalysisContext<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExecutablePlan, SQLError> {
        control.check()?;
        let (source, predicate) = input_parts(self.query, self.inline, self.relation, control)?;
        let plan = match plan {
            MutableExecutablePlan::Query(query) => MutableExecutablePlan::Query(attach_query(
                query,
                source,
                predicate,
                self.condition,
                control,
            )?),
            MutableExecutablePlan::Command(command) => {
                let command = match &*command {
                    CommandPlan::Insert(_) => {
                        let command =
                            ValuesInput::prepare_action_with_control(context, command, control)?;
                        command.map_insert_source(control, |_, query| {
                            attach_query(
                                query.expect("INSERT source"),
                                source,
                                predicate,
                                self.condition,
                                control,
                            )
                        })?
                    }
                    CommandPlan::Update(_) | CommandPlan::Delete(_) => {
                        let mut command = command;
                        if let Some(source) = source {
                            command = command.map_source_input(control, |original| {
                                prepend(original, Some(source), control)
                            })?;
                        }
                        for condition in [predicate, self.condition].into_iter().flatten() {
                            command = command.map_predicate_input(control, |original, arena| {
                                add_condition(original, arena, condition, control)
                            })?;
                        }
                        command
                    }
                    _ => {
                        return Err(SQLError::Internal(
                            "rule query input requires a relational action".into(),
                        ))
                    }
                };
                MutableExecutablePlan::Command(command)
            }
        };
        substitute_plan_inputs_owned(plan, &self.expressions, control)
    }
}

fn input_parts(
    query: MutableQueryPlan,
    inline: bool,
    relation: InternalRelationId,
    control: &ProductionControl<'_>,
) -> Result<(Option<MutableSourcePlan>, Option<MutableExpressionPlan>), SQLError> {
    Ok(if inline {
        let mut query = query;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            return Err(SQLError::Internal(
                "rule target list lost its query block".into(),
            ));
        };
        if !query.ctes.is_empty() {
            return Err(SQLError::Internal(
                "rule target list has an unexpected local CTE scope".into(),
            ));
        }
        let count = block.subqueries.len();
        let subject = query
            .relation_privilege_subject
            .as_ref()
            .map(|subject| subject.clone_with_control(control))
            .transpose()?;
        if let Some(subject) = subject {
            for index in 0..count {
                query =
                    query.map_query_child(QueryChildSlot::Subquery(index), control, |query| {
                        if query.relation_privilege_subject.is_none() {
                            query.set_privilege_subject(
                                Some(subject.clone_with_control(control)?),
                                control,
                            )
                        } else {
                            Ok(query)
                        }
                    })?;
            }
        }
        let (query, predicate) = query.take_where_expression(control)?;
        let (_, source) = query.take_source(control)?;
        (source, predicate)
    } else {
        let columns = control.finish(Vec::new(), control.empty_reservation())?;
        (
            Some(MutableSourcePlan::subquery(
                query,
                None,
                columns,
                Some(relation),
                false,
                control,
            )?),
            None,
        )
    })
}

fn attach_query(
    query: MutableQueryPlan,
    source: Option<MutableSourcePlan>,
    predicate: Option<MutableExpressionPlan>,
    condition: Option<MutableExpressionPlan>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let query = query.project_values(control)?;
    if matches!(query.root, RelationalPlan::SetOp { .. }) {
        if source.is_some() || predicate.is_some() || condition.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "conditional UNION/INTERSECT/EXCEPT statements are not implemented".into(),
            });
        }
        return Ok(query);
    }
    let mut query = query;
    if let Some(source) = source {
        let (parent, original) = query.take_source(control)?;
        query = parent.replace_source(prepend(original, Some(source), control)?, control)?;
    }
    for condition in [predicate, condition].into_iter().flatten() {
        query = query.map_where_input(control, |original, arena| {
            add_condition(original, arena, condition, control)
        })?;
    }
    Ok(query)
}

fn prepend(
    original: Option<MutableSourcePlan>,
    input: Option<MutableSourcePlan>,
    control: &ProductionControl<'_>,
) -> Result<Option<MutableSourcePlan>, SQLError> {
    Ok(match (original, input) {
        (Some(original), Some(input)) => Some(MutableSourcePlan::cross_join(
            input, original, true, control,
        )?),
        (original, None) => original,
        (None, input) => input,
    })
}

fn add_condition(
    original: Option<OwnedScalarExpr>,
    arena: &mut ScalarQueryArena,
    condition: MutableExpressionPlan,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    let condition = arena.append_expression(condition, control)?;
    Ok(Some(match original {
        Some(original) => {
            OwnedScalarExpr::boolean([original, condition].into_iter(), true, control)?
        }
        None => condition,
    }))
}
