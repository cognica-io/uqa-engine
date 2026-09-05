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
use uqa_sql::ast::{InsertStmt, OnConflictAction};

pub(super) fn bind_insert(
    insert: &InsertStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<InsertStmt, SQLError> {
    let mut output = insert.clone();
    output.with = bind_ctes(&insert.with, resolver, inherited, context)?;
    let context = context.with_ctes(&insert.with)?;
    output.rows = insert
        .rows
        .iter()
        .map(|row| bind_expanding_exprs(row, resolver, inherited, &context))
        .collect::<Result<Vec<_>, SQLError>>()?;
    output.select_source = insert
        .select_source
        .as_deref()
        .map(|select| bind_select_with_scope(select, resolver, inherited, &context).map(Box::new))
        .transpose()?;
    output.on_conflict = insert
        .on_conflict
        .as_ref()
        .map(|conflict| -> Result<uqa_sql::ast::OnConflict, SQLError> {
            let mut conflict_scope = inherited.clone();
            conflict_scope.insert_qualifier(&insert.target_qualifier);
            Ok(uqa_sql::ast::OnConflict {
                predicate: conflict
                    .predicate
                    .as_deref()
                    .map(|expr| {
                        bind_rule_expr_with_scope(expr, resolver, &conflict_scope, &context)
                            .map(Box::new)
                    })
                    .transpose()?,
                constraint: conflict.constraint.clone(),
                conflict_columns: conflict.conflict_columns.clone(),
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
                                    column.clone(),
                                    bind_rule_expr_with_scope(
                                        expr,
                                        resolver,
                                        &conflict_scope,
                                        &context,
                                    )?,
                                ))
                            })
                            .collect::<Result<Vec<_>, SQLError>>()?,
                        r#where: bind_optional_expr(
                            r#where.as_deref(),
                            resolver,
                            &conflict_scope,
                            &context,
                        )?
                        .map(Box::new),
                    },
                },
            })
        })
        .transpose()?;
    output.returning.clear();
    Ok(output)
}
