//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Carry what binding recorded for stored syntax back into that syntax: exact routine identities and ordering, the OID identities of user-defined cast types, and the enum constants that binding converted from `unknown` literals.

use super::{
    routines::apply_routine_reference, BTreeSet, Expr, SQLError, Statement, StoredAstVisitor,
};
use crate::ast::UserTypeIdentity;
use crate::binding::syntax_sites::{SyntaxSites, ValueSite};
use uqa_core::Value;

/// Apply the sites of a bound copy of `expression`. Returns whether the syntax changed.
pub fn bind_stored_expression_sites(
    expression: &mut Expr,
    sites: &SyntaxSites,
) -> Result<bool, SQLError> {
    apply_sites(sites, Syntax::Expression(expression))
}

/// Apply the sites of a bound copy of `statement`. Returns whether the syntax changed.
pub fn bind_stored_statement_sites(
    statement: &mut Statement,
    sites: &SyntaxSites,
) -> Result<bool, SQLError> {
    apply_sites(sites, Syntax::Statement(statement))
}

enum Syntax<'a> {
    Expression(&'a mut Expr),
    Statement(&'a mut Statement),
}

fn apply_sites(sites: &SyntaxSites, syntax: Syntax<'_>) -> Result<bool, SQLError> {
    let mut routines = sites.routines.iter();
    let mut values = sites.values.iter().peekable();
    let mut routines_changed = false;
    let mut values_changed = false;
    let mut relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut routine = |name: &mut String,
                       binding: Option<&mut Option<crate::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        let reference = routines.next().ok_or_else(|| {
            SQLError::Internal(format!(
                "stored catalog routine binding has no entry for call `{name}`"
            ))
        })?;
        routines_changed |= apply_routine_reference(name, binding, reference)?;
        Ok(())
    };
    let mut expression = |node: &mut Expr| -> Result<(), SQLError> {
        values_changed |= apply_value_site(node, &mut values)?;
        Ok(())
    };
    let mut visitor = StoredAstVisitor {
        source: None,
        merge: None,
        expression: Some(&mut expression),
        projection: None,
        ty: None,
        relation: &mut relation,
        routine: &mut routine,
    };
    match syntax {
        Syntax::Expression(expression) => visitor.bind_expr(expression, &BTreeSet::new())?,
        Syntax::Statement(statement) => visitor.bind_statement(statement)?,
    }
    if let Some(reference) = routines.next() {
        return Err(SQLError::Internal(format!(
            "stored catalog routine binding entry `{}` has no matching call",
            reference.name
        )));
    }
    if values.next().is_some() {
        return Err(SQLError::Internal(
            "stored catalog binding has an expression site without matching syntax".into(),
        ));
    }
    Ok(routines_changed || values_changed)
}

fn apply_value_site<'a>(
    node: &mut Expr,
    sites: &mut std::iter::Peekable<impl Iterator<Item = &'a ValueSite>>,
) -> Result<bool, SQLError> {
    let mismatch = |what: &str| {
        SQLError::Internal(format!(
            "stored catalog binding does not match the {what} of its syntax"
        ))
    };
    // A relabel wraps the node, whose own site the visitor reads when it descends into the wrapped node.
    if let Some(ValueSite::Relabel(ty)) = sites.peek() {
        let ty = (*ty).clone();
        sites.next();
        let inner = std::mem::replace(node, Expr::Literal(Value::Null));
        *node = Expr::Cast {
            implicit: true,
            expr: Box::new(inner),
            ty,
        };
        return Ok(true);
    }
    match node {
        Expr::Cast { ty, .. } => {
            let Some(ValueSite::Cast(bound)) = sites.next() else {
                return Err(mismatch("cast"));
            };
            // Built-in type names keep their written spelling; a user-defined type is named by identity.
            if UserTypeIdentity::parse(bound).is_some() && ty != bound {
                ty.clone_from(bound);
                return Ok(true);
            }
            Ok(false)
        }
        Expr::Literal(Value::Str(_) | Value::Null) => match sites.next() {
            Some(ValueSite::Literal) => Ok(false),
            Some(ValueSite::Constant { value, ty }) => {
                *node = Expr::TypedLiteral {
                    value: value.clone(),
                    ty: ty.clone(),
                };
                Ok(true)
            }
            _ => Err(mismatch("literal")),
        },
        Expr::Func { order_syntax, .. } => {
            let Some(ValueSite::FunctionOrder(bound)) = sites.next() else {
                return Err(mismatch("function ordering"));
            };
            if order_syntax == bound {
                return Ok(false);
            }
            if !order_syntax.is_legacy() {
                return Err(mismatch("function ordering"));
            }
            *order_syntax = *bound;
            Ok(true)
        }
        _ => match sites.next() {
            Some(ValueSite::Node) => Ok(false),
            _ => Err(mismatch("expression")),
        },
    }
}

#[cfg(test)]
mod tests;

/// Assignment of a stored `unknown` literal to `target`, as `coerce_to_target_type` converts an untyped constant with the target type's input function when a default, generation expression or result is analyzed: the literal becomes a constant of the enum or enum-array type, or of that base type of a domain, whose own check applies when the value is assigned. Any other expression is coerced when it is evaluated.
pub fn fold_assigned_stored_literal(
    expression: &mut Expr,
    target: &crate::ast::ColumnType,
    catalog: Option<&dyn crate::expr::enums::EnumLabelCatalog>,
) -> Result<bool, SQLError> {
    let Expr::Literal(value @ (Value::Str(_) | Value::Null)) = expression else {
        return Ok(false);
    };
    let mut base = target;
    while let crate::ast::ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    if !crate::expr::enums::is_enum_bearing(base) {
        return Ok(false);
    }
    let Some(catalog) = catalog else {
        return Ok(false);
    };
    let Some(value) = crate::expr::enums::fold_unknown_literal(Some(catalog), value, base)? else {
        return Ok(false);
    };
    *expression = Expr::TypedLiteral {
        value,
        ty: base.catalog_name(),
    };
    Ok(true)
}
