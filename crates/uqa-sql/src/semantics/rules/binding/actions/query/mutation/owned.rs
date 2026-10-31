//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation event inputs retain independent target, source and expression owners through action attachment.

use super::RuleMutation;
use crate::{
    ast::{Expr, InternalRelationId},
    binding::correlation::{expression_depends_on_outer_row_with_control, CorrelationContext},
    catalog::roles::RoleReference,
    plan::{
        input_projection::{substitute_expression_inputs_owned, OwnedInputProjections},
        ExpressionPlan, MutableExpressionPlan, MutableQueryPlan, MutableSourcePlan,
    },
    semantics::rules::{
        analysis::{rule_returning_columns_with_control, RuleAnalysisContext},
        binding::{OwnedRuleQueryInput, OwnedRuleQueryResolver},
    },
    RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::{OwnedMap, Produced, ProductionControl};

mod capture;
mod namespaces;

pub struct OwnedMutationRuleInput {
    pub resolver: OwnedRuleQueryResolver,
    query: MutableQueryPlan,
    relation: InternalRelationId,
    pub source_context: Produced<Vec<Expr>>,
    pub source_context_schema: Option<RowSchema>,
}

impl OwnedMutationRuleInput {
    #[expect(
        clippy::too_many_arguments,
        reason = "keeps mutation syntax, physical schemas, authority and request ownership distinct"
    )]
    pub fn bind(
        context: RuleAnalysisContext<'_>,
        correlation: CorrelationContext<'_>,
        statement: RuleMutation<'_>,
        schemas: (&RowSchema, Option<&RowSchema>),
        references_event: bool,
        input_subject: &RoleReference,
        rule_subject: &RoleReference,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let (target_schema, source_schema) = schemas;
        let relation = InternalRelationId::allocate();
        let source_relation = InternalRelationId::allocate();
        let captured = statement
            .source()
            .zip(source_schema)
            .map(|(source, schema)| {
                capture::source(
                    statement,
                    source,
                    schema,
                    source_relation,
                    input_subject,
                    control,
                )
            })
            .transpose()?;
        let empty_columns = OwnedMap::new();
        let source_columns = captured
            .as_ref()
            .map_or(&empty_columns, |source| &source.columns);
        let definitions = rule_returning_columns_with_control(context, statement.table(), control)?;
        let target_width = definitions.len();
        let rewrite = namespaces::input_rewrite(
            statement,
            &definitions,
            source_schema,
            relation,
            source_relation,
            target_schema,
            control,
        )?;
        let inputs = MutationExpressions {
            target: relation,
            source: source_relation,
            target_width,
            source_columns,
            rewrite: &rewrite,
            subject: input_subject,
        };
        let resolver = OwnedRuleQueryResolver::from_mutation_with_control(
            context,
            statement,
            &definitions,
            Some(relation),
            Some(&rewrite),
            control,
            |expression| inputs.bind(expression, control),
        )?;
        let needs_target = needs_target(
            correlation,
            statement,
            references_event,
            target_width,
            control,
        )?;
        let predicate = inputs.predicate(statement, control)?;
        let target = needs_target
            .then(|| {
                capture::target(
                    statement,
                    &definitions,
                    target_schema,
                    relation,
                    rule_subject,
                    control,
                )
            })
            .transpose()?;
        let (source, source_context, source_context_schema) = match captured {
            Some(source) => (Some(source.source), source.context, Some(source.schema)),
            None => (
                None,
                control.finish(Vec::new(), control.empty_reservation())?,
                None,
            ),
        };
        let query =
            capture::action_query(statement, target, source, predicate, input_subject, control)?;
        Ok(Self {
            resolver,
            query,
            relation,
            source_context,
            source_context_schema,
        })
    }

    pub fn finish(
        self,
        condition: Option<MutableExpressionPlan>,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedRuleQueryInput, SQLError> {
        Ok(OwnedRuleQueryInput {
            query: self.query,
            inline: true,
            relation: self.relation,
            condition,
            expressions: control.finish(self.resolver.expressions, control.empty_reservation())?,
        })
    }
}

struct MutationExpressions<'a> {
    target: InternalRelationId,
    source: InternalRelationId,
    target_width: usize,
    source_columns: &'a OwnedInputProjections,
    rewrite: &'a crate::semantics::view_rewrite::DeferredOuterRewrite,
    subject: &'a RoleReference,
}

impl MutationExpressions<'_> {
    fn bind(
        &self,
        expression: MutableExpressionPlan,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExpressionPlan, SQLError> {
        let expression = bind_positions(
            expression,
            self.target,
            self.source,
            self.target_width,
            control,
        )?;
        self.substitute(expression, control)
    }

    fn substitute(
        &self,
        expression: MutableExpressionPlan,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExpressionPlan, SQLError> {
        let expression =
            substitute_expression_inputs_owned(expression, self.source_columns, control)?;
        super::super::resolver::retain_authority(expression, Some(self.subject), control)
    }

    fn predicate(
        &self,
        statement: RuleMutation<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<MutableExpressionPlan>, SQLError> {
        statement
            .predicate()
            .map(|scalar| {
                let expression =
                    MutableExpressionPlan::copy_from(scalar, statement.subqueries(), control)?;
                let expression = bind_positions(
                    expression,
                    self.target,
                    self.source,
                    self.target_width,
                    control,
                )?;
                let expression = self.rewrite.bind_expression_owned(expression, control)?;
                self.substitute(expression, control)?
                    .prune_subqueries(control)
            })
            .transpose()
    }
}

fn needs_target(
    correlation: CorrelationContext<'_>,
    statement: RuleMutation<'_>,
    references_event: bool,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    Ok(references_event
        || references_target_position(statement.predicate(), width, control)?
        || statement
            .predicate()
            .map(|scalar| {
                expression_depends_on_outer_row_with_control(
                    correlation,
                    scalar,
                    statement.subqueries(),
                    statement.source(),
                    statement.ctes(),
                    control,
                )
            })
            .transpose()?
            .unwrap_or(false))
}

fn references_target_position(
    predicate: Option<&ScalarExpr>,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut found = false;
    if let Some(predicate) = predicate {
        predicate.try_visit(&mut |node| {
            control.check()?;
            found |= matches!(node, ScalarExpr::Position(position) if *position < width);
            Ok::<_, SQLError>(!found)
        })?;
    }
    Ok(found)
}

fn bind_positions(
    expression: MutableExpressionPlan,
    target: InternalRelationId,
    source: InternalRelationId,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    expression.map_scalar(control, |scalar, _| {
        scalar.transform_nodes(control, &mut |_, node| {
            if let ScalarExpr::Position(position) = &*node {
                let column = if *position < width {
                    target.column(*position)
                } else {
                    source.column(*position - width)
                };
                Ok(ScalarExpr::InternalColumn(column).copy_for_rewrite(control)?)
            } else {
                Ok(node)
            }
        })
    })
}

fn input_expression(
    relation: InternalRelationId,
    position: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<ExpressionPlan>, SQLError> {
    MutableExpressionPlan::copy_from(
        &ScalarExpr::InternalColumn(relation.column(position)),
        &[],
        control,
    )?
    .finish(control)
}

struct CapturedSource {
    source: MutableSourcePlan,
    columns: OwnedInputProjections,
    context: Produced<Vec<Expr>>,
    schema: RowSchema,
}
