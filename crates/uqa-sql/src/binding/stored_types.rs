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

/// Resolve the name of a user-defined type, with any array suffixes, to its identity; `None` leaves the name as it is.
pub type TypeIdentityResolver<'a> =
    dyn FnMut(&str, TypeNameSite) -> Result<Option<String>, SQLError> + 'a;

/// How a stored type name was spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeNameSite {
    /// As written or as its type's SQL name renders it, quoted where the name needs it: casts, typed constants and routine declarations.
    Written,
    /// Folded to lower case even inside quotes, as routine bindings and routine dependencies record argument types.
    Canonical,
}

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
        if let ScalarExpr::Cast { ty, .. }
        | ScalarExpr::TypedLiteral { ty, .. }
        | ScalarExpr::CompositeRow {
            binding: crate::ast::CompositeRowBinding { ty, .. },
            ..
        } = node
        {
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
    bind_plan_type_identities(|rewrite| plan.rewrite_scalar_expressions(rewrite), resolve)
}

/// Replace the names of user-defined types in the casts and typed constants of a compiled statement with their identities.
pub fn bind_unified_plan_type_identities(
    plan: &mut crate::plan::UnifiedPlan,
    resolve: &mut TypeNameResolver<'_>,
) -> Result<(), SQLError> {
    bind_plan_type_identities(|rewrite| plan.rewrite_scalar_expressions(rewrite), resolve)
}

fn bind_plan_type_identities(
    visit: impl FnOnce(&mut dyn FnMut(&mut ScalarExpr)),
    resolve: &mut TypeNameResolver<'_>,
) -> Result<(), SQLError> {
    let mut failure = None;
    visit(&mut |expression| {
        if failure.is_some() {
            return;
        }
        if let ScalarExpr::Cast { ty, .. }
        | ScalarExpr::TypedLiteral { ty, .. }
        | ScalarExpr::CompositeRow {
            binding: crate::ast::CompositeRowBinding { ty, .. },
            ..
        } = expression
        {
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
        projection: None,
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

/// Replace a user-defined type name spelled as `site` records it with its identity; returns whether it changed.
pub fn upgrade_type_name(
    name: &mut String,
    site: TypeNameSite,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let upgrade = TypeNameUpgrade::new(resolve);
    upgrade.rename_at(name, site);
    upgrade.finish()
}

/// Replace the user-defined type names a routine binding records with their identities; returns whether anything changed.
pub fn upgrade_function_binding_type_names(
    binding: &mut crate::ast::FunctionBinding,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let upgrade = TypeNameUpgrade::new(resolve);
    upgrade.binding(binding);
    upgrade.finish()
}

/// A name-to-identity rewrite over one stored definition, shared by the callbacks that find its type names, which records whether anything changed and the first resolution failure.
struct TypeNameUpgrade<'r, 'a> {
    resolve: std::cell::RefCell<&'r mut TypeIdentityResolver<'a>>,
    changed: std::cell::Cell<bool>,
    failure: std::cell::RefCell<Option<SQLError>>,
}

impl<'r, 'a> TypeNameUpgrade<'r, 'a> {
    fn new(resolve: &'r mut TypeIdentityResolver<'a>) -> Self {
        Self {
            resolve: std::cell::RefCell::new(resolve),
            changed: std::cell::Cell::new(false),
            failure: std::cell::RefCell::new(None),
        }
    }

    fn rename(&self, name: &mut String) {
        self.rename_at(name, TypeNameSite::Written);
    }

    fn rename_at(&self, name: &mut String, site: TypeNameSite) {
        if self.failure.borrow().is_some() {
            return;
        }
        if UserTypeIdentity::parse(name).is_some() || ColumnType::from_sql_name(name).is_ok() {
            return;
        }
        match (self.resolve.borrow_mut())(name, site) {
            Ok(Some(identity)) => {
                *name = identity;
                self.changed.set(true);
            }
            Ok(None) => {}
            Err(error) => *self.failure.borrow_mut() = Some(error),
        }
    }

    /// A routine binding's argument types and its invocation's coercion targets, source types, parameter types and result type.
    fn binding(&self, binding: &mut crate::ast::FunctionBinding) {
        let canonical = |name: &mut String| self.rename_at(name, TypeNameSite::Canonical);
        binding.argument_types.iter_mut().for_each(canonical);
        if let Some(invocation) = &mut binding.invocation {
            invocation.argument_targets.iter_mut().for_each(canonical);
            invocation
                .argument_sources
                .iter_mut()
                .flatten()
                .for_each(canonical);
            invocation.parameter_types.iter_mut().for_each(canonical);
            invocation.return_type.iter_mut().for_each(canonical);
        }
    }

    fn finish(self) -> Result<bool, SQLError> {
        match self.failure.into_inner() {
            Some(error) => Err(error),
            None => Ok(self.changed.get()),
        }
    }
}

/// Replace the names of user-defined types that earlier releases recorded in stored syntax, in casts, typed constants and routine bindings, with their identities. Returns whether anything changed.
pub fn upgrade_stored_statement_type_names(
    statement: &mut Statement,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let upgrade = TypeNameUpgrade::new(resolve);
    upgrade_syntax(&upgrade, |visitor| visitor.bind_statement(statement))?;
    upgrade.finish()
}

/// [`upgrade_stored_statement_type_names`] for one stored expression.
pub fn upgrade_stored_expression_type_names(
    expression: &mut Expr,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let upgrade = TypeNameUpgrade::new(resolve);
    upgrade_syntax(&upgrade, |visitor| {
        visitor.bind_expr(expression, &BTreeSet::new())
    })?;
    upgrade.finish()
}

/// The routine callback of an upgrade, which rewrites each call's binding.
type BindingCallback<'a> = &'a mut dyn FnMut(
    &mut String,
    Option<&mut Option<crate::ast::FunctionBinding>>,
) -> Result<(), SQLError>;

fn upgrade_syntax(
    upgrade: &TypeNameUpgrade<'_, '_>,
    visit: impl FnOnce(
        &mut StoredAstVisitor<'_, fn(&mut String) -> Result<(), SQLError>, BindingCallback<'_>>,
    ) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let mut relation: fn(&mut String) -> Result<(), SQLError> = |_| Ok(());
    let mut rewrite_binding =
        |_: &mut String, binding: Option<&mut Option<crate::ast::FunctionBinding>>| {
            if let Some(Some(binding)) = binding {
                upgrade.binding(binding);
            }
            Ok(())
        };
    let mut routine: BindingCallback<'_> = &mut rewrite_binding;
    let mut ty = |name: &mut String| upgrade.rename(name);
    visit(&mut StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        projection: None,
        ty: Some(&mut ty),
        relation: &mut relation,
        routine: &mut routine,
    })
}

/// [`upgrade_stored_statement_type_names`] for a stored query plan: the casts, typed constants and bindings of its scalars and the bindings of its table functions. Column definition lists keep the names they were written with, as definition does.
pub fn upgrade_query_plan_type_names(
    plan: &mut QueryPlan,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let upgrade = TypeNameUpgrade::new(resolve);
    plan.rewrite_scalar_expressions(&mut |expression| upgrade_scalar(&upgrade, expression));
    plan.visit_sources_mut(&mut |source| match source {
        crate::plan::SourcePlan::Function {
            binding: Some(binding),
            ..
        } => upgrade.binding(binding),
        crate::plan::SourcePlan::FunctionGroup { functions, .. } => {
            for binding in functions
                .iter_mut()
                .filter_map(|function| function.binding.as_mut())
            {
                upgrade.binding(binding);
            }
        }
        _ => {}
    });
    upgrade.finish()
}

/// [`upgrade_query_plan_type_names`] for a stored expression plan and its subqueries.
pub fn upgrade_expression_plan_type_names(
    plan: &mut crate::plan::ExpressionPlan,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let mut changed = false;
    for subquery in &mut plan.subqueries {
        changed |= upgrade_query_plan_type_names(subquery, resolve)?;
    }
    let upgrade = TypeNameUpgrade::new(resolve);
    crate::plan::rewrite_scalar_expression(&mut plan.scalar, &mut |expression| {
        upgrade_scalar(&upgrade, expression);
    });
    Ok(upgrade.finish()? || changed)
}

fn upgrade_scalar(upgrade: &TypeNameUpgrade<'_, '_>, expression: &mut ScalarExpr) {
    match expression {
        ScalarExpr::Cast { ty, .. }
        | ScalarExpr::TypedLiteral { ty, .. }
        | ScalarExpr::CompositeRow {
            binding: crate::ast::CompositeRowBinding { ty, .. },
            ..
        } => upgrade.rename(ty),
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } => upgrade.binding(binding),
        _ => {}
    }
}
