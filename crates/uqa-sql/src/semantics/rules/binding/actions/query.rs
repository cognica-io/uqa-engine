//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind rule actions to their query input before that input starts producing rows.

use crate::{
    ast::{Expr, InternalRelationId, Statement},
    plan::{
        AccessPathPlan, AggregateClassifier, CommandPlan, ComputePlan, JoinExecutionStrategy,
        ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
    },
    plpgsql::{ResolvedVariable, VariableResolver},
    SQLError, ScalarExpr,
};
use std::collections::{BTreeMap, BTreeSet};

mod schedule;
pub use schedule::InsertRuleQueries;
mod assignments;
mod conditions;
mod mutation;
pub use mutation::{MutationRuleInput, RuleMutation};
#[cfg(test)]
mod tests;
mod values;

/// An action can borrow a native query tree without converting its rows into constants.
pub enum RuleAction {
    Stored(Box<Statement>),
    Rewritten(UnifiedPlan),
}

impl From<Statement> for RuleAction {
    fn from(statement: Statement) -> Self {
        Self::Stored(Box::new(statement))
    }
}

impl RuleAction {
    /// Bind native inputs with the rule's catalog metadata before capturing execution state.
    pub fn bind(
        context: crate::semantics::rules::analysis::RuleAnalysisContext<'_>,
        aggregates: &dyn AggregateClassifier,
        statement: Statement,
        input: Option<RuleQueryInput>,
    ) -> Result<Self, SQLError> {
        let mut plan = Self::from(statement).lower(aggregates)?;
        if let Some(input) = input {
            input.attach(&mut plan, context)?;
        }
        Ok(Self::Rewritten(plan))
    }

    pub fn lower_with_control(
        self,
        aggregates: &dyn AggregateClassifier,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<crate::plan::MutableExecutablePlan, SQLError> {
        match self {
            Self::Stored(statement) => {
                UnifiedPlan::lower_borrowed_for_rewrite(&statement, aggregates, control)?
                    .mark_catalog_relations_bound(control)
            }
            Self::Rewritten(plan) => plan.copy_for_rewrite(control),
        }
    }

    pub fn lower(self, aggregates: &dyn AggregateClassifier) -> Result<UnifiedPlan, SQLError> {
        match self {
            Self::Stored(statement) => {
                let mut plan = UnifiedPlan::lower_with(*statement, aggregates);
                crate::binding::stored_routines::mark_catalog_statement_relations_bound(&mut plan)?;
                Ok(plan)
            }
            Self::Rewritten(plan) => Ok(plan),
        }
    }
}

#[derive(Clone)]
pub struct RuleQueryInput {
    pub query: QueryPlan,
    pub inline: bool,
    pub relation: InternalRelationId,
    pub condition: Option<crate::plan::ExpressionPlan>,
    pub expressions: BTreeMap<crate::ast::InternalColumnRef, crate::plan::ExpressionPlan>,
}

impl RuleQueryInput {
    fn attach(
        self,
        plan: &mut UnifiedPlan,
        context: crate::semantics::rules::analysis::RuleAnalysisContext<'_>,
    ) -> Result<(), SQLError> {
        let (source, predicate) = if self.inline {
            let mut query = self.query;
            let RelationalPlan::QueryBlock(block) = &mut query.root else {
                return Err(SQLError::Internal(
                    "rule target list lost its query block".into(),
                ));
            };
            if !query.ctes.is_empty() {
                return Err(SQLError::Internal(
                    "rule target list has an unexpected local CTE scope".into(),
                ));
            }
            values::retain_authority(
                &mut block.subqueries,
                query.relation_privilege_subject.as_ref(),
            );
            let predicate = block
                .r#where
                .take()
                .map(|scalar| crate::plan::ExpressionPlan {
                    scalar,
                    subqueries: std::mem::take(&mut block.subqueries),
                });
            (block.from.take(), predicate)
        } else {
            (
                Some(SourcePlan::Subquery {
                    captured_output: Some(self.relation),
                    lateral: false,
                    body: Box::new(self.query),
                    alias: None,
                    column_aliases: Vec::new(),
                }),
                None,
            )
        };
        match plan {
            UnifiedPlan::Query(query) => attach_query(query, source, predicate, self.condition),
            UnifiedPlan::Command(command) => match command.as_mut() {
                CommandPlan::Insert(insert) => {
                    if insert.source.is_none() {
                        let mut values =
                            values::ValuesInput::describe(insert)?.expect("VALUES action input");
                        insert.source = Some(values.source(context, insert)?);
                        values.project_layer(context, insert)?;
                    }
                    attach_query(
                        insert.source.as_mut().expect("insert source"),
                        source,
                        predicate,
                        self.condition,
                    )
                }
                CommandPlan::Update(update) => {
                    update.source = prepend(update.source.take().map(|s| *s), source).map(Box::new);
                    add_native_condition(&mut update.predicate, &mut update.subqueries, predicate);
                    add_native_condition(
                        &mut update.predicate,
                        &mut update.subqueries,
                        self.condition,
                    );
                    Ok(())
                }
                CommandPlan::Delete(delete) => {
                    delete.source = prepend(delete.source.take().map(|s| *s), source).map(Box::new);
                    add_native_condition(&mut delete.predicate, &mut delete.subqueries, predicate);
                    add_native_condition(
                        &mut delete.predicate,
                        &mut delete.subqueries,
                        self.condition,
                    );
                    Ok(())
                }
                _ => Err(SQLError::Internal(
                    "rule query input requires a relational action".into(),
                )),
            },
        }?;
        crate::plan::input_projection::substitute_plan_inputs(plan, &self.expressions)
    }
}

fn attach_query(
    query: &mut QueryPlan,
    source: Option<SourcePlan>,
    predicate: Option<crate::plan::ExpressionPlan>,
    condition: Option<crate::plan::ExpressionPlan>,
) -> Result<(), SQLError> {
    if let RelationalPlan::Values { rows, subqueries } = &mut query.root {
        if rows.len() == 1 {
            let mut block = projection_block(
                rows.pop()
                    .expect("one VALUES row")
                    .into_iter()
                    .map(|expr| ProjectionPlan { expr, alias: None })
                    .collect(),
            );
            block.subqueries = std::mem::take(subqueries);
            query.root = RelationalPlan::QueryBlock(Box::new(block));
        }
    }
    if let RelationalPlan::Values { rows, subqueries } = &mut query.root {
        let relation = InternalRelationId::allocate();
        let width = rows.first().map_or(0, Vec::len);
        let mut block = projection_block(
            (0..width)
                .map(|position| ProjectionPlan {
                    expr: ScalarExpr::InternalColumn(relation.column(position)),
                    alias: None,
                })
                .collect(),
        );
        block.from = Some(SourcePlan::Values {
            lateral: true,
            rows: std::mem::take(rows),
            alias: None,
            column_aliases: Vec::new(),
            internal_relation: Some(relation),
            internal_column_types: vec![None; width],
            internal_column_collations: vec![None; width],
        });
        block.subqueries = std::mem::take(subqueries);
        query.root = RelationalPlan::QueryBlock(Box::new(block));
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            block.from = prepend(block.from.take(), source);
            add_native_condition(&mut block.r#where, &mut block.subqueries, predicate);
            add_native_condition(&mut block.r#where, &mut block.subqueries, condition);
            Ok(())
        }
        RelationalPlan::SetOp { .. }
            if source.is_some() || predicate.is_some() || condition.is_some() =>
        {
            Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "conditional UNION/INTERSECT/EXCEPT statements are not implemented".into(),
            })
        }
        RelationalPlan::SetOp { .. } => Ok(()),
        RelationalPlan::Values { .. } => unreachable!("VALUES lowered to a relational source"),
    }
}

fn add_condition(
    predicate: &mut Option<ScalarExpr>,
    subqueries: &mut Vec<QueryPlan>,
    condition: Option<Expr>,
    subject: Option<&crate::catalog::roles::RoleReference>,
) {
    if let Some(condition) = condition {
        let mut expression = crate::plan::ExpressionPlan::lower(condition);
        for query in &mut expression.subqueries {
            crate::binding::stored_routines::mark_query_relations_bound(query);
            query.relation_privilege_subject = subject.cloned();
        }
        let offset = subqueries.len();
        crate::plan::rewrite_scalar_expression(&mut expression.scalar, &mut |node| match node {
            ScalarExpr::ScalarSubquery(index)
            | ScalarExpr::Exists {
                subquery: index, ..
            }
            | ScalarExpr::InSubquery {
                subquery: index, ..
            } => *index += offset,
            _ => {}
        });
        subqueries.extend(expression.subqueries);
        *predicate = Some(match predicate.take() {
            Some(previous) => ScalarExpr::And(vec![previous, expression.scalar]),
            None => expression.scalar,
        });
    }
}

fn add_native_condition(
    predicate: &mut Option<ScalarExpr>,
    subqueries: &mut Vec<QueryPlan>,
    input: Option<crate::plan::ExpressionPlan>,
) {
    if let Some(input) = input {
        let input = crate::plan::input_projection::append_expression(subqueries, input);
        *predicate = Some(match predicate.take() {
            Some(previous) => ScalarExpr::And(vec![previous, input]),
            None => input,
        });
    }
}

fn prepend(existing: Option<SourcePlan>, source: Option<SourcePlan>) -> Option<SourcePlan> {
    let Some(source) = source else {
        return existing;
    };
    Some(match existing {
        Some(right) => SourcePlan::Join {
            comma: false,
            left: Box::new(source),
            right: Box::new(right),
            kind: crate::ast::JoinKind::Cross,
            on: None,
            using: None,
            natural: false,
            alias: None,
            column_aliases: Vec::new(),
            lateral: true,
            strategy: JoinExecutionStrategy::Auto,
        },
        None => source,
    })
}

fn projection_block(projections: Vec<ProjectionPlan>) -> QueryBlockPlan {
    QueryBlockPlan {
        deferred_outer_rewrites: Vec::new(),
        windows: Vec::new(),
        projections,
        privilege_columns: uqa_core::memory::OwnedSet::new(),
        from: None,
        r#where: None,
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: Vec::new(),
        limit: None,
        with_ties: false,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: Vec::new(),
        access: AccessPathPlan::Row,
        locking: Vec::new(),
    }
}

/// NEW fields are expressions over the input query, so unused fields never run.
pub struct RuleQueryResolver {
    pub table: String,
    pub columns: Vec<(String, Result<Expr, SQLError>)>,
    pub old: BTreeMap<String, Expr>,
    pub expressions: BTreeMap<crate::ast::InternalColumnRef, crate::plan::ExpressionPlan>,
}

impl VariableResolver for RuleQueryResolver {
    fn rewrite_with_control(
        &mut self,
        reference: crate::plpgsql::RewriteReference<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Expr>>, SQLError> {
        use crate::{plan::copy::SyntaxProduction, plpgsql::RewriteReference};
        control.check()?;
        let production = SyntaxProduction::new(*control);
        let expression = match reference {
            RewriteReference::Name(name) | RewriteReference::WholeRow(name) => {
                let new = name.eq_ignore_ascii_case("new");
                if !new && !name.eq_ignore_ascii_case("old") {
                    return Ok(None);
                }
                let mut items = production.vector(self.columns.len())?;
                for (name, expression) in &self.columns {
                    let expression = if new {
                        expression.as_ref().map_err(Clone::clone)?
                    } else {
                        &self.old[name]
                    };
                    items.push(production.copy(expression)?)?;
                }
                Expr::Cast {
                    implicit: true,
                    expr: production.boxed(Expr::Row(items.finish()?))?,
                    ty: production.copy(&self.table)?,
                }
            }
            RewriteReference::Qualified { qualifier, column } => {
                let expression = if qualifier.eq_ignore_ascii_case("new") {
                    self.columns
                        .iter()
                        .find(|(name, _)| name == column)
                        .map(|(_, value)| value.as_ref().map_err(Clone::clone))
                        .transpose()?
                } else if qualifier.eq_ignore_ascii_case("old") {
                    self.old.get(column)
                } else {
                    return Ok(None);
                };
                production
                    .copy(expression.ok_or_else(|| {
                        SQLError::UnknownColumn(format!("{qualifier}.{column}"))
                    })?)?
            }
            RewriteReference::Parameter(_) | RewriteReference::Internal(_) => return Ok(None),
        };
        Ok(Some(production.finish(expression)?))
    }

    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        let items = if name.eq_ignore_ascii_case("new") {
            self.columns
                .iter()
                .map(|(_, expression)| expression.clone())
                .collect::<Result<Vec<_>, _>>()?
        } else if name.eq_ignore_ascii_case("old") {
            self.columns
                .iter()
                .map(|(name, _)| self.old[name].clone())
                .collect()
        } else {
            return Ok(None);
        };
        Ok(Some(Expr::Cast {
            implicit: true,
            expr: Box::new(Expr::Row(items)),
            ty: self.table.clone(),
        }))
    }
    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        if qualifier.eq_ignore_ascii_case("new") {
            self.columns
                .iter()
                .find(|(name, _)| name == column)
                .map(|(_, expression)| expression.clone().map(Some))
                .unwrap_or_else(|| Err(SQLError::UnknownColumn(format!("{qualifier}.{column}"))))
        } else if qualifier.eq_ignore_ascii_case("old") {
            self.old
                .get(column)
                .cloned()
                .map(Some)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{qualifier}.{column}")))
        } else {
            Ok(None)
        }
    }
    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.rewrite_name(qualifier)
    }
}

impl RuleQueryResolver {
    /// Describe the source positions referenced by an action and its condition before planner pruning.
    pub fn input_columns(
        &self,
        context: crate::semantics::rules::analysis::RuleAnalysisContext<'_>,
        insert: &crate::plan::InsertPlan,
        action: &Statement,
        condition: Option<&Expr>,
        target_columns: &BTreeSet<String>,
    ) -> Result<Option<(BTreeSet<usize>, usize)>, SQLError> {
        use crate::semantics::rules::action_binding as binding;
        let mut required =
            binding::rule_statement_row_columns(context.sources, action, target_columns)?;
        if let Some(condition) = condition {
            required.extend(binding::rule_expr_row_columns(condition));
        }
        if binding::rule_statement_references_whole_row(context.sources, action, target_columns)?
            || condition.is_some_and(binding::rule_expr_references_whole_row)
        {
            return Ok(None);
        }
        let supplied = if insert.columns.is_empty() {
            let width = insert
                .source
                .as_deref()
                .and_then(|query| query.analyzed_output.as_ref())
                .map_or_else(
                    || inline_width(insert).unwrap_or(self.columns.len()),
                    crate::RowSchema::len,
                );
            self.columns
                .iter()
                .take(width)
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
        } else {
            insert
                .columns
                .iter()
                .map(|column| column.column.as_str())
                .collect()
        };
        let positions = supplied
            .iter()
            .enumerate()
            .filter_map(|(index, name)| required.contains(*name).then_some(index))
            .collect();
        Ok(Some((positions, supplied.len())))
    }

    pub fn from_insert(
        context: crate::semantics::rules::analysis::RuleAnalysisContext<'_>,
        insert: &crate::plan::InsertPlan,
        relation: InternalRelationId,
    ) -> Result<Self, SQLError> {
        let definitions =
            crate::semantics::rules::analysis::rule_returning_columns(context, &insert.table)?;
        let supplied = if insert.columns.is_empty() {
            let width = insert
                .source
                .as_deref()
                .and_then(|source| source.analyzed_output.as_ref())
                .map_or_else(
                    || inline_width(insert).unwrap_or(definitions.len()),
                    crate::RowSchema::len,
                );
            definitions
                .iter()
                .take(width)
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>()
        } else {
            insert
                .columns
                .iter()
                .map(|column| column.column.as_str())
                .collect()
        };
        let mut columns = Vec::with_capacity(definitions.len());
        let mut old = BTreeMap::new();
        let mut expressions = BTreeMap::new();
        for column in &definitions {
            let ty = column.ty.catalog_name();
            let null = Expr::Cast {
                implicit: true,
                expr: Box::new(Expr::Literal(uqa_core::Value::Null)),
                ty: ty.clone(),
            };
            old.insert(column.name.clone(), null.clone());
            let metadata = crate::semantics::rules::binding::RuleColumnMetadata {
                ty: column.ty.clone(),
                collation_oid: column.collation_oid,
            };
            if insert
                .columns
                .iter()
                .any(|target| target.column == column.name && !target.is_whole_column())
            {
                let reference = InternalRelationId::allocate().column(0);
                let mut expression =
                    assignments::column_expression(context, insert, column, relation)?;
                expression.scalar = metadata.bind_input(expression.scalar);
                expressions.insert(reference, expression);
                columns.push((column.name.clone(), Ok(Expr::InternalColumn(reference))));
                continue;
            }
            let supplied_position = supplied
                .iter()
                .position(|name| *name == column.name)
                .filter(|_| !values::overrides_identity(column, insert.overriding));
            let value = if let Some(position) = supplied_position {
                Expr::InternalColumn(relation.column(position))
            } else {
                values::default_expression(context, column)?
                    .unwrap_or(Expr::Literal(uqa_core::Value::Null))
            };
            let mut expression = crate::plan::ExpressionPlan::lower(Expr::Cast {
                implicit: true,
                expr: Box::new(value),
                ty,
            });
            expression.scalar = metadata.bind_input(expression.scalar);
            let reference = InternalRelationId::allocate().column(0);
            expressions.insert(reference, expression);
            columns.push((column.name.clone(), Ok(Expr::InternalColumn(reference))));
        }
        let inputs = inline_input_expressions(insert, relation)?;
        for expression in expressions.values_mut() {
            crate::plan::input_projection::substitute_expression_inputs(expression, &inputs)?;
        }
        expressions.extend(inputs);
        if let Some(subject) = insert
            .source
            .as_ref()
            .and_then(|query| query.relation_privilege_subject.as_ref())
        {
            for expression in expressions.values_mut() {
                for query in &mut expression.subqueries {
                    query
                        .relation_privilege_subject
                        .get_or_insert(subject.clone());
                }
            }
        }
        Ok(Self {
            table: insert.table.clone(),
            columns,
            old,
            expressions,
        })
    }
}

fn inline_width(insert: &crate::plan::InsertPlan) -> Option<usize> {
    if !insert
        .rule_values_input
        .as_ref()
        .is_some_and(crate::plan::RuleValuesInput::is_inline)
    {
        return None;
    }
    match &insert.source.as_ref()?.root {
        RelationalPlan::QueryBlock(block) => Some(block.projections.len()),
        _ => None,
    }
}

fn inline_input_expressions(
    insert: &crate::plan::InsertPlan,
    relation: InternalRelationId,
) -> Result<BTreeMap<crate::ast::InternalColumnRef, crate::plan::ExpressionPlan>, SQLError> {
    let mut inputs = BTreeMap::new();
    if !insert
        .rule_values_input
        .as_ref()
        .is_some_and(crate::plan::RuleValuesInput::is_inline)
    {
        return Ok(inputs);
    }
    let source = insert.source.as_ref().expect("inline input query");
    let RelationalPlan::QueryBlock(block) = &source.root else {
        return Err(SQLError::Internal(
            "rule target list lost its query block".into(),
        ));
    };
    for (position, projection) in block.projections.iter().enumerate() {
        let mut input = crate::plan::ExpressionPlan {
            scalar: projection.expr.clone(),
            subqueries: block.subqueries.clone(),
        };
        crate::plan::subqueries::prune_expression(&mut input);
        values::retain_authority(
            &mut input.subqueries,
            source.relation_privilege_subject.as_ref(),
        );
        inputs.insert(relation.column(position), input);
    }
    Ok(inputs)
}
