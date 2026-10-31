//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Binding for INSERT conflict targets and actions in durable rewrite rules.

use super::{
    bind_ctes, bind_expanding_exprs, bind_optional_expr, bind_rule_expr_with_scope,
    bind_select_with_scope, RuleBindingContext, RuleBindingScope, SQLError, VariableResolver,
};
use crate::ast::{InsertStmt, OnConflictAction};
use crate::plan::copy::CollectSyntax;

pub(super) fn bind_insert(
    insert: &InsertStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &crate::plan::copy::SyntaxProduction<'_>,
) -> Result<InsertStmt, SQLError> {
    let with = bind_ctes(&insert.with, resolver, inherited, context, production)?;
    let context = context.with_ctes(&insert.with)?;
    let columns = insert
        .columns
        .iter()
        .map(|target| {
            super::bind_assignment_target(target, resolver, inherited, &context, production)
        })
        .collect_syntax(production)?;
    let rows = insert
        .rows
        .iter()
        .map(|row| bind_expanding_exprs(row, resolver, inherited, &context, production))
        .collect_syntax(production)?;
    let select_source = insert
        .select_source
        .as_deref()
        .map(|select| {
            bind_select_with_scope(select, resolver, inherited, &context, production)
                .and_then(|value| production.boxed(value))
        })
        .transpose()?;
    let on_conflict = insert
        .on_conflict
        .as_ref()
        .map(|conflict| bind_conflict(conflict, insert, resolver, inherited, &context, production))
        .transpose()?;
    Ok(InsertStmt {
        table: production.copy(&insert.table)?,
        target_relation_bound: insert.target_relation_bound,
        target_qualifier: production.copy(&insert.target_qualifier)?,
        target_alias: production.copy(&insert.target_alias)?,
        include_descendants: insert.include_descendants,
        overriding: insert.overriding,
        with,
        columns,
        rows,
        select_source,
        on_conflict,
        returning: Vec::new(),
        returning_aliases: production.copy(&insert.returning_aliases)?,
    })
}

fn bind_conflict(
    conflict: &crate::ast::OnConflict,
    insert: &InsertStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &crate::plan::copy::SyntaxProduction<'_>,
) -> Result<crate::ast::OnConflict, SQLError> {
    let mut conflict_scope = inherited.copy(&context.control())?;
    conflict_scope.insert_qualifier(&insert.target_qualifier, &context.control())?;
    Ok(crate::ast::OnConflict {
        predicate: conflict
            .predicate
            .as_deref()
            .map(|expr| {
                bind_rule_expr_with_scope(expr, resolver, &conflict_scope, context, production)
                    .and_then(|value| production.boxed(value))
            })
            .transpose()?,
        constraint: production.copy(&conflict.constraint)?,
        conflict_columns: production.copy(&conflict.conflict_columns)?,
        expressions: conflict
            .expressions
            .iter()
            .map(|expr| {
                bind_rule_expr_with_scope(expr, resolver, &conflict_scope, context, production)
            })
            .collect_syntax(production)?,
        action: match &conflict.action {
            OnConflictAction::Nothing => OnConflictAction::Nothing,
            OnConflictAction::Update {
                assignments,
                r#where,
            } => OnConflictAction::Update {
                assignments: assignments
                    .iter()
                    .map(|(column, expr)| {
                        Ok((
                            super::bind_assignment_targets(
                                column,
                                resolver,
                                &conflict_scope,
                                context,
                                production,
                            )?,
                            bind_rule_expr_with_scope(
                                expr,
                                resolver,
                                &conflict_scope,
                                context,
                                production,
                            )?,
                        ))
                    })
                    .collect_syntax(production)?,
                r#where: bind_optional_expr(
                    r#where.as_deref(),
                    resolver,
                    &conflict_scope,
                    context,
                    production,
                )?
                .map(|value| production.boxed(value))
                .transpose()?,
            },
        },
    })
}
