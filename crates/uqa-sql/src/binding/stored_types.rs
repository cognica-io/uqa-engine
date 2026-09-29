//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored plans and stored syntax record the user-defined types they cast to by OID identity, as `PostgreSQL` stores type OIDs, so a rename, a schema move or another search path cannot change which type they mean.

use crate::ast::{ColumnType, Expr, Statement, UserTypeIdentity};
use crate::catalog::stored_ast::StoredAstVisitor;
use crate::ir::ScalarExpr;
use crate::plan::QueryPlan;
use crate::SQLError;
use std::collections::BTreeSet;

/// Resolve a type name as the statement's binding did.
pub type TypeNameResolver<'a> = dyn FnMut(&str) -> Result<Option<ColumnType>, SQLError> + 'a;

fn identity(name: &str, resolve: &mut TypeNameResolver<'_>) -> Result<Option<String>, SQLError> {
    if UserTypeIdentity::parse(name).is_some() || ColumnType::from_sql_name(name).is_ok() {
        return Ok(None);
    }
    Ok(resolve(name)?.and_then(|ty| ty.user_type_identity()))
}

/// Replace the names of user-defined types in the casts and typed constants of one stored scalar expression with their identities.
pub fn bind_scalar_type_identities(
    expression: &mut ScalarExpr,
    resolve: &mut TypeNameResolver<'_>,
) -> Result<(), SQLError> {
    let mut failure = None;
    crate::plan::rewrite_scalar_expression(expression, &mut |node| {
        if failure.is_some() {
            return;
        }
        if let ScalarExpr::Cast { ty, .. } | ScalarExpr::TypedLiteral { ty, .. } = node {
            match identity(ty, resolve) {
                Ok(Some(identity)) => *ty = identity,
                Ok(None) => {}
                Err(error) => failure = Some(error),
            }
        }
    });
    failure.map_or(Ok(()), Err)
}

/// Replace the names of user-defined types in the casts and typed constants of a stored plan with their identities.
pub fn bind_query_plan_type_identities(
    plan: &mut QueryPlan,
    resolve: &mut TypeNameResolver<'_>,
) -> Result<(), SQLError> {
    let mut failure = None;
    plan.rewrite_scalar_expressions(&mut |expression| {
        if failure.is_some() {
            return;
        }
        if let ScalarExpr::Cast { ty, .. } | ScalarExpr::TypedLiteral { ty, .. } = expression {
            match identity(ty, resolve) {
                Ok(Some(identity)) => *ty = identity,
                Ok(None) => {}
                Err(error) => failure = Some(error),
            }
        }
    });
    failure.map_or(Ok(()), Err)
}

fn visit_type_names(
    resolve: &mut TypeNameResolver<'_>,
    visit: impl FnOnce(
        &mut StoredAstVisitor<
            '_,
            fn(&mut String) -> Result<(), SQLError>,
            fn(
                &mut String,
                Option<&mut Option<crate::ast::FunctionBinding>>,
            ) -> Result<(), SQLError>,
        >,
    ) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let mut failure = None;
    let mut relation: fn(&mut String) -> Result<(), SQLError> = |_| Ok(());
    let mut routine: fn(
        &mut String,
        Option<&mut Option<crate::ast::FunctionBinding>>,
    ) -> Result<(), SQLError> = |_, _| Ok(());
    let mut rename = |name: &mut String| {
        if failure.is_some() {
            return;
        }
        match identity(name, resolve) {
            Ok(Some(identity)) => *name = identity,
            Ok(None) => {}
            Err(error) => failure = Some(error),
        }
    };
    visit(&mut StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        ty: Some(&mut rename),
        relation: &mut relation,
        routine: &mut routine,
    })?;
    failure.map_or(Ok(()), Err)
}

/// Replace the names of user-defined types in the casts and typed constants of stored syntax with their identities.
pub fn bind_stored_expression_type_identities(
    expression: &mut Expr,
    resolve: &mut TypeNameResolver<'_>,
) -> Result<(), SQLError> {
    visit_type_names(resolve, |visitor| {
        visitor.bind_expr(expression, &BTreeSet::new())
    })
}

/// Replace the names of user-defined types in the casts and typed constants of a stored statement with their identities.
pub fn bind_stored_statement_type_identities(
    statement: &mut Statement,
    resolve: &mut TypeNameResolver<'_>,
) -> Result<(), SQLError> {
    visit_type_names(resolve, |visitor| visitor.bind_statement(statement))
}
