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

/// An action can borrow a native query tree without converting its rows into constants.
pub struct RuleAction {
    pub statement: Statement,
    pub input: Option<RuleQueryInput>,
}

impl From<Statement> for RuleAction {
    fn from(statement: Statement) -> Self {
        Self {
            statement,
            input: None,
        }
    }
}

impl RuleAction {
    pub fn lower(self, aggregates: &dyn AggregateClassifier) -> Result<UnifiedPlan, SQLError> {
        let mut plan = UnifiedPlan::lower_with(self.statement, aggregates);
        crate::binding::stored_routines::mark_catalog_statement_relations_bound(&mut plan)?;
        if let Some(input) = self.input {
            input.attach(&mut plan)?;
        }
        Ok(plan)
    }
}

#[derive(Clone)]
pub struct RuleQueryInput {
    pub query: QueryPlan,
    pub relation: InternalRelationId,
    pub condition: Option<Expr>,
    pub expressions: BTreeMap<crate::ast::InternalColumnRef, crate::plan::ExpressionPlan>,
}

impl RuleQueryInput {
    fn attach(self, plan: &mut UnifiedPlan) -> Result<(), SQLError> {
        let source = SourcePlan::Subquery {
            captured_output: Some(self.relation),
            lateral: false,
            body: Box::new(self.query),
            alias: None,
            column_aliases: Vec::new(),
        };
        match plan {
            UnifiedPlan::Query(query) => attach_query(query, source, self.condition),
            UnifiedPlan::Command(command) => match command.as_mut() {
                CommandPlan::Insert(insert) => {
                    if insert.source.is_none() {
                        insert.source = Some(Box::new(QueryPlan {
                            analyzed_output: None,
                            analyzed_source: None,
                            analyzed_grouping: None,
                            relation_privilege_subject: None,
                            relations_bound: insert.relations_bound,
                            ctes: Vec::new(),
                            root: RelationalPlan::Values {
                                rows: std::mem::take(&mut insert.rows),
                                subqueries: insert.subqueries.clone(),
                            },
                        }));
                    }
                    attach_query(
                        insert.source.as_mut().expect("insert source"),
                        source,
                        self.condition,
                    )
                }
                CommandPlan::Update(update) => {
                    update.source =
                        Some(Box::new(prepend(update.source.take().map(|s| *s), source)));
                    add_condition(
                        &mut update.predicate,
                        &mut update.subqueries,
                        self.condition,
                        None,
                    );
                    Ok(())
                }
                CommandPlan::Delete(delete) => {
                    delete.source =
                        Some(Box::new(prepend(delete.source.take().map(|s| *s), source)));
                    add_condition(
                        &mut delete.predicate,
                        &mut delete.subqueries,
                        self.condition,
                        None,
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
    source: SourcePlan,
    condition: Option<Expr>,
) -> Result<(), SQLError> {
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
            block.from = Some(prepend(block.from.take(), source));
            add_condition(&mut block.r#where, &mut block.subqueries, condition, None);
            Ok(())
        }
        RelationalPlan::SetOp { left, right, .. } => {
            attach_query(left, source.clone(), condition.clone())?;
            attach_query(right, source, condition)
        }
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

fn prepend(existing: Option<SourcePlan>, source: SourcePlan) -> SourcePlan {
    match existing {
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
    }
}

fn projection_block(projections: Vec<ProjectionPlan>) -> QueryBlockPlan {
    QueryBlockPlan {
        deferred_outer_rewrites: Vec::new(),
        windows: Vec::new(),
        projections,
        privilege_columns: BTreeSet::new(),
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
pub struct InsertQueryResolver {
    pub table: String,
    pub columns: Vec<(String, Expr)>,
    pub old: BTreeMap<String, Expr>,
    pub expressions: BTreeMap<crate::ast::InternalColumnRef, crate::plan::ExpressionPlan>,
}

impl VariableResolver for InsertQueryResolver {
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
                .collect()
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
                .map(|(_, expression)| Some(expression.clone()))
                .ok_or_else(|| SQLError::UnknownColumn(format!("{qualifier}.{column}")))
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

impl InsertQueryResolver {
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
                .map_or(self.columns.len(), crate::RowSchema::len);
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

    pub fn new(
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
                .map_or(definitions.len(), crate::RowSchema::len);
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
            if insert
                .columns
                .iter()
                .any(|target| target.column == column.name && !target.is_whole_column())
            {
                let reference = InternalRelationId::allocate().column(0);
                expressions.insert(
                    reference,
                    assignments::column_expression(context, insert, column, relation)?,
                );
                columns.push((column.name.clone(), Expr::InternalColumn(reference)));
                continue;
            }
            let value = supplied
                .iter()
                .position(|name| *name == column.name)
                .map_or_else(
                    || {
                        column
                            .default
                            .clone()
                            .unwrap_or(Expr::Literal(uqa_core::Value::Null))
                    },
                    |position| Expr::InternalColumn(relation.column(position)),
                );
            columns.push((
                column.name.clone(),
                Expr::Cast {
                    implicit: true,
                    expr: Box::new(value),
                    ty,
                },
            ));
        }
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
