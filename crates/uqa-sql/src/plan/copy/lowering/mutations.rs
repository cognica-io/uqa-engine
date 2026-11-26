//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation roots reuse ordered field constructors and retain completed children through suspended input.

use super::{
    expressions::construct_optional_scalar,
    input::text,
    projections::construct_projections,
    query::construct::{Input, Source as QuerySource},
    rows::construct_rows,
    Copying, Result,
};
use crate::{
    ast::{DeleteStmt, FromClause, InsertStmt, MergeStmt, SelectStmt, UpdateStmt},
    construction::{retain, retain_with, Read},
    plan::{
        retirement::{construction::Prefix, Pending},
        CommandPlan, DeletePlan, InsertPlan, MergePlan, QueryPlan, SourcePlan, UpdatePlan,
    },
};

mod assignments;
mod inline;

#[derive(Clone, Copy)]
pub(in crate::plan::copy::lowering) enum Source<'s> {
    Insert(&'s InsertStmt),
    Update(&'s UpdateStmt),
    Delete(&'s DeleteStmt),
    Merge(&'s MergeStmt),
}

impl<'s> Source<'s> {
    pub(in crate::plan::copy::lowering) fn statement(
        statement: &'s crate::ast::Statement,
    ) -> Option<Self> {
        use crate::ast::Statement;
        Some(match statement {
            Statement::Insert(value) => Self::Insert(value),
            Statement::Update(value) => Self::Update(value),
            Statement::Delete(value) => Self::Delete(value),
            Statement::Merge(value) => Self::Merge(value),
            _ => return None,
        })
    }
    pub(in crate::plan::copy::lowering) fn cte(source: &'s crate::ast::CteBody) -> Self {
        use crate::ast::CteBody;
        match source {
            CteBody::Insert(value) => Self::Insert(value),
            CteBody::Update(value) => Self::Update(value),
            CteBody::Delete(value) => Self::Delete(value),
            CteBody::Merge(value) => Self::Merge(value),
            CteBody::Query(_) => unreachable!("query selected by CTE constructor"),
        }
    }
}

pub(in crate::plan::copy::lowering) async fn construct<'s, 'c, I: Input<'s, 'c>>(
    source: Source<'s>,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<CommandPlan>> {
    let command = match source {
        Source::Insert(statement) => {
            copying.admit_box::<InsertPlan>()?;
            let value = retain_with(copying.control, || {
                construct_insert(statement, copying, input)
            })?
            .await?;
            CommandPlan::Insert(copying.finish_box(value, Prefix::take)?)
        }
        Source::Update(statement) => {
            copying.admit_box::<UpdatePlan>()?;
            let value = retain_with(copying.control, || {
                construct_update(statement, copying, input)
            })?
            .await?;
            CommandPlan::Update(copying.finish_box(value, Prefix::take)?)
        }
        Source::Delete(statement) => {
            copying.admit_box::<DeletePlan>()?;
            let value = retain_with(copying.control, || {
                construct_delete(statement, copying, input)
            })?
            .await?;
            CommandPlan::Delete(copying.finish_box(value, Prefix::take)?)
        }
        Source::Merge(statement) => {
            copying.admit_box::<MergePlan>()?;
            let value = retain_with(copying.control, || {
                construct_merge(statement, copying, input)
            })?
            .await?;
            CommandPlan::Merge(copying.finish_box(value, Prefix::take)?)
        }
    };
    Ok(copying.retain_plan(command))
}

pub(in crate::plan::copy::lowering) async fn construct_insert<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s InsertStmt,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<InsertPlan>> {
    input.checkpoint().await?;
    copying.check()?;
    let ctes = field!(copying, "ctes", |field| Read::new(
        field.control,
        input.ctes(&statement.with, &mut field)
    )?
    .await?);
    let source = field!(copying, "source", |field| query(
        statement.select_source.as_deref(),
        &mut field,
        input
    )
    .await?);
    let rows = Pending::new(field!(copying, "rows", |field| retain(
        field.control,
        construct_rows(
            &statement.rows,
            input,
            field.control,
            field.fields.as_mut().expect("row records"),
            field.memory.as_mut().expect("row allowance")
        )
    )?
    .await?));
    let on_conflict = field!(
        copying,
        "on_conflict",
        |field| match &statement.on_conflict {
            Some(conflict) => Pending::new(Some(
                retain(
                    field.control,
                    assignments::construct_conflict(conflict, &mut field, input)
                )?
                .await?
                .take()
            )),
            None => Pending::new(None),
        }
    );
    let returning = field!(copying, "returning", |field| retain(
        field.control,
        construct_projections(&statement.returning, &mut field, input)
    )?
    .await?);
    let table = field!(copying, "table", |field| field
        .take(input.text(&statement.table, field.control).await?));
    let target_privilege_subject = copying.field("target_privilege_subject", |_| Ok(None))?;
    let target_qualifier = copying.take(
        input
            .text(&statement.target_qualifier, copying.control)
            .await?,
    );
    let target_alias =
        text::optional_text(statement.target_alias.as_deref(), copying, input).await?;
    let columns = field!(copying, "columns", |field| retain(
        field.control,
        assignments::construct_targets(&statement.columns, &mut field, input)
    )?
    .await?);
    let rule_values_input = copying.field("rule_values_input", |_| Ok(None))?;
    let returning_aliases =
        text::returning_aliases(&statement.returning_aliases, copying, input).await?;
    let subqueries = Read::new(copying.control, input.finish_arena(copying))?.await?;
    let view_checks = copying.field("view_checks", |_| Ok(Vec::new()))?;
    let view_checks = Pending::new(view_checks);
    let view_rule_relations = copying.field("view_rule_relations", |_| Ok(Vec::new()))?;
    let view_rule_insert_plans = copying.field("view_rule_insert_plans", |_| Ok(Vec::new()))?;
    let view_rule_returning = copying.field("view_rule_returning", |_| Ok(None))?;
    Ok(copying.retain_plan(InsertPlan {
        prepared_domain_defaults: None,
        table,
        target_relation_bound: statement.target_relation_bound,
        relations_bound: false,
        statement_privilege_subject: None,
        target_privilege_subject,
        target_qualifier,
        target_alias,
        include_descendants: statement.include_descendants,
        columns: columns.take(),
        overriding: statement.overriding,
        ctes: ctes.take(),
        rows: rows.take(),
        source: source.take(),
        rule_values_input,
        on_conflict: on_conflict.take(),
        returning: returning.take(),
        returning_aliases,
        subqueries: subqueries.take(),
        view_checks: view_checks.take(),
        view_rule_relations,
        view_rule_insert_plans,
        view_rule_returning,
    }))
}

pub(in crate::plan::copy::lowering) async fn construct_update<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s UpdateStmt,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<UpdatePlan>> {
    input.checkpoint().await?;
    copying.check()?;
    let ctes = field!(copying, "ctes", |field| Read::new(
        field.control,
        input.ctes(&statement.with, &mut field)
    )?
    .await?);
    let source = field!(copying, "source", |field| optional_source(
        statement.from.as_ref(),
        &mut field,
        input
    )
    .await?);
    let assignments = field!(copying, "assignments", |field| {
        retain(
            field.control,
            assignments::construct_assignments(&statement.assignments, &mut field, input),
        )?
        .await?
    });
    let predicate = field!(copying, "predicate", |field| retain(
        field.control,
        construct_optional_scalar(statement.r#where.as_ref(), &mut field, input)
    )?
    .await?);
    let returning = field!(copying, "returning", |field| retain(
        field.control,
        construct_projections(&statement.returning, &mut field, input)
    )?
    .await?);
    let table = field!(copying, "table", |field| field
        .take(input.text(&statement.table, field.control).await?));
    let target_privilege_subject = copying.field("target_privilege_subject", |_| Ok(None))?;
    let target_qualifier = copying.take(
        input
            .text(&statement.target_qualifier, copying.control)
            .await?,
    );
    let target_alias =
        text::optional_text(statement.target_alias.as_deref(), copying, input).await?;
    let returning_aliases =
        text::returning_aliases(&statement.returning_aliases, copying, input).await?;
    let subqueries = Read::new(copying.control, input.finish_arena(copying))?.await?;
    let view_checks = copying.field("view_checks", |_| Ok(Vec::new()))?;
    let view_checks = Pending::new(view_checks);
    let view_rule_relations = copying.field("view_rule_relations", |_| Ok(Vec::new()))?;
    let view_rule_update_plans = copying.field("view_rule_update_plans", |_| Ok(Vec::new()))?;
    let view_rule_returning = copying.field("view_rule_returning", |_| Ok(None))?;
    Ok(copying.retain_plan(UpdatePlan {
        prepared_domain_defaults: None,
        table,
        target_relation_bound: statement.target_relation_bound,
        relations_bound: false,
        statement_privilege_subject: None,
        target_privilege_subject,
        target_qualifier,
        target_alias,
        include_descendants: statement.include_descendants,
        assignments: assignments.take(),
        predicate: predicate.take(),
        ctes: ctes.take(),
        source: source.take(),
        returning: returning.take(),
        returning_aliases,
        subqueries: subqueries.take(),
        view_checks: view_checks.take(),
        view_rule_relations,
        view_rule_update_plans,
        view_rule_returning,
    }))
}

pub(in crate::plan::copy::lowering) async fn construct_delete<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s DeleteStmt,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<DeletePlan>> {
    input.checkpoint().await?;
    copying.check()?;
    let ctes = field!(copying, "ctes", |field| Read::new(
        field.control,
        input.ctes(&statement.with, &mut field)
    )?
    .await?);
    let source = field!(copying, "source", |field| optional_source(
        statement.using.as_ref(),
        &mut field,
        input
    )
    .await?);
    let predicate = field!(copying, "predicate", |field| retain(
        field.control,
        construct_optional_scalar(statement.r#where.as_ref(), &mut field, input)
    )?
    .await?);
    let returning = field!(copying, "returning", |field| retain(
        field.control,
        construct_projections(&statement.returning, &mut field, input)
    )?
    .await?);
    let table = field!(copying, "table", |field| field
        .take(input.text(&statement.table, field.control).await?));
    let target_privilege_subject = copying.field("target_privilege_subject", |_| Ok(None))?;
    let target_qualifier = copying.take(
        input
            .text(&statement.target_qualifier, copying.control)
            .await?,
    );
    let target_alias =
        text::optional_text(statement.target_alias.as_deref(), copying, input).await?;
    let returning_aliases =
        text::returning_aliases(&statement.returning_aliases, copying, input).await?;
    let subqueries = Read::new(copying.control, input.finish_arena(copying))?.await?;
    let view_rule_relations = copying.field("view_rule_relations", |_| Ok(Vec::new()))?;
    let view_rule_returning = copying.field("view_rule_returning", |_| Ok(None))?;
    Ok(copying.retain_plan(DeletePlan {
        table,
        target_relation_bound: statement.target_relation_bound,
        relations_bound: false,
        statement_privilege_subject: None,
        target_privilege_subject,
        target_qualifier,
        target_alias,
        include_descendants: statement.include_descendants,
        predicate: predicate.take(),
        ctes: ctes.take(),
        source: source.take(),
        returning: returning.take(),
        returning_aliases,
        subqueries: subqueries.take(),
        view_rule_relations,
        view_rule_returning,
    }))
}

pub(in crate::plan::copy::lowering) async fn construct_merge<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s MergeStmt,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<MergePlan>> {
    input.checkpoint().await?;
    copying.check()?;
    let source = field!(copying, "source", |field| source(
        &statement.source,
        &mut field,
        input
    )
    .await?);
    let join_condition = field!(copying, "join_condition", |field| Pending::new(
        field.take_scalar(
            input
                .scalar(&statement.join_condition, field.control)
                .await?
        )?
    ));
    let when_clauses = field!(copying, "when_clauses", |field| {
        retain(
            field.control,
            assignments::construct_merge_whens(&statement.when_clauses, &mut field, input),
        )?
        .await?
    });
    let returning = field!(copying, "returning", |field| retain(
        field.control,
        construct_projections(&statement.returning, &mut field, input)
    )?
    .await?);
    let ctes = field!(copying, "ctes", |field| Read::new(
        field.control,
        input.ctes(&statement.with, &mut field)
    )?
    .await?);
    let target = field!(copying, "target", |field| field
        .take(input.text(&statement.target, field.control).await?));
    let target_privilege_subject = copying.field("target_privilege_subject", |_| Ok(None))?;
    let target_qualifier = copying.take(
        input
            .text(&statement.target_qualifier, copying.control)
            .await?,
    );
    let target_alias =
        text::optional_text(statement.target_alias.as_deref(), copying, input).await?;
    let target_predicate = copying.field("target_predicate", |_| Ok(None))?;
    let target_predicate = Pending::new(target_predicate);
    let returning_aliases =
        text::returning_aliases(&statement.returning_aliases, copying, input).await?;
    let subqueries = Read::new(copying.control, input.finish_arena(copying))?.await?;
    let view_checks = copying.field("view_checks", |_| Ok(Vec::new()))?;
    let view_checks = Pending::new(view_checks);
    Ok(copying.retain_plan(MergePlan {
        prepared_domain_defaults: None,
        ctes: ctes.take(),
        target,
        statement_privilege_subject: None,
        target_privilege_subject,
        target_qualifier,
        target_alias,
        include_descendants: statement.include_descendants,
        target_predicate: target_predicate.take(),
        source: source.take(),
        join_condition: join_condition.take(),
        when_clauses: when_clauses.take(),
        returning: returning.take(),
        returning_aliases,
        subqueries: subqueries.take(),
        view_checks: view_checks.take(),
    }))
}

async fn query<'s, 'c, I: Input<'s, 'c>>(
    statement: Option<&'s SelectStmt>,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<Option<Box<QueryPlan>>>> {
    let query = match statement {
        Some(statement) => {
            copying.admit_box::<QueryPlan>()?;
            let query = Read::new(
                copying.control,
                input.query(QuerySource::Select(statement), copying),
            )?
            .await?;
            Some(copying.finish_box(query, Prefix::take)?)
        }
        None => None,
    };
    Ok(copying.retain_plan(query))
}

async fn source<'s, 'c, I: Input<'s, 'c>>(
    source: &'s FromClause,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<Box<SourcePlan>>> {
    copying.admit_box::<SourcePlan>()?;
    let source = Read::new(copying.control, input.source(source, copying))?.await?;
    Ok(copying.retain_plan(copying.finish_box(source, Prefix::take)?))
}

async fn optional_source<'s, 'c, I: Input<'s, 'c>>(
    value: Option<&'s FromClause>,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<Option<Box<SourcePlan>>>> {
    let source = match value {
        Some(value) => Some(source(value, copying, input).await?.take()),
        None => None,
    };
    Ok(copying.retain_plan(source))
}
