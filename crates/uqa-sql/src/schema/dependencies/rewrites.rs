//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite stored relation, column, and literal sequence references in SQL schema expressions.
use super::walk_schema_expr_mut;
use uqa_core::{RelationIdentity, Value};

pub fn stored_relation_reference_matches(reference: &str, target: &RelationIdentity) -> bool {
    match RelationIdentity::parse_reference(reference) {
        Ok((Some(schema), name)) => schema == target.schema && name == target.name,
        Ok((None, name)) => name == target.name,
        // Corrupt legacy metadata is never evidence that a dependency is absent; DDL must not leave it dangling.
        Err(_) => true,
    }
}

pub fn upgrade_legacy_schema_function_dispatches(
    columns: &mut [crate::ast::ColumnDef],
    constraints: &mut crate::ast::TableConstraintSet,
) -> bool {
    let mut changed = false;
    for column in columns {
        for expression in [column.default.as_mut(), column.check.as_mut()]
            .into_iter()
            .flatten()
        {
            changed |= expression.upgrade_legacy_serialized_dispatches();
        }
        if let Some(generated) = &mut column.generated {
            changed |= generated.expression.upgrade_legacy_serialized_dispatches();
        }
    }
    for check in &mut constraints.checks {
        changed |= check.expr.upgrade_legacy_serialized_dispatches();
    }
    changed
}

pub fn rewrite_sequence_function_references(
    expression: &mut crate::ast::Expr,
    visit: &mut impl FnMut(&mut String) -> Result<(), String>,
) -> Result<(), String> {
    walk_schema_expr_mut(expression, &mut |node| {
        let crate::ast::Expr::Func { name, args, .. } = node else {
            return Ok(());
        };
        let lower = name.to_ascii_lowercase();
        let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
        if !matches!(local, "nextval" | "currval" | "setval")
            || (lower.contains('.') && !lower.starts_with("pg_catalog."))
        {
            return Ok(());
        }
        let Some(reference) = args.first_mut().and_then(regclass_literal_mut) else {
            // Dynamically computed text arguments retain late binding; literal regclass spellings identify catalog dependencies at declaration time.
            return Ok(());
        };
        visit(reference)
    })
}

fn regclass_literal_mut(expression: &mut crate::ast::Expr) -> Option<&mut String> {
    match expression {
        crate::ast::Expr::Literal(Value::Str(reference)) => Some(reference),
        crate::ast::Expr::Cast { expr, ty }
            if ty.eq_ignore_ascii_case("regclass")
                || ty.eq_ignore_ascii_case("pg_catalog.regclass") =>
        {
            regclass_literal_mut(expr)
        }
        _ => None,
    }
}

pub fn rename_schema_expr_column(
    expression: &mut crate::ast::Expr,
    from: &str,
    to: &str,
) -> Result<(), String> {
    walk_schema_expr_mut(expression, &mut |node| {
        match node {
            crate::ast::Expr::Star | crate::ast::Expr::QualifiedStar(_) => {
                return Err("schema expression contains `*` and cannot be rewritten safely".into());
            }
            crate::ast::Expr::Column(name) if name == from => *name = to.to_string(),
            crate::ast::Expr::QualifiedColumn { column, .. } if column == from => {
                *column = to.to_string();
            }
            _ => {}
        }
        Ok(())
    })
}

pub fn schema_expr_references_relation(
    expression: &crate::ast::Expr,
    target: &RelationIdentity,
) -> bool {
    let mut expression = expression.clone();
    let mut referenced = false;
    let result = walk_schema_expr_mut(&mut expression, &mut |node| {
        if let crate::ast::Expr::QualifiedColumn { qualifier, .. } = node {
            referenced |= stored_relation_reference_matches(qualifier, target);
        }
        Ok(())
    });
    result.is_err() || referenced
}

pub fn rename_schema_expr_relation(
    expression: &mut crate::ast::Expr,
    from: &RelationIdentity,
    to: &str,
) -> Result<(), String> {
    walk_schema_expr_mut(expression, &mut |node| {
        if let crate::ast::Expr::QualifiedColumn { qualifier, .. } = node {
            if stored_relation_reference_matches(qualifier, from) {
                *qualifier = to.to_string();
            }
        }
        Ok(())
    })
}

pub fn rename_schema_expr_qualified_column(
    expression: &mut crate::ast::Expr,
    table: &RelationIdentity,
    from: &str,
    to: &str,
) -> Result<(), String> {
    walk_schema_expr_mut(expression, &mut |node| {
        if let crate::ast::Expr::QualifiedColumn { qualifier, column } = node {
            if column == from && stored_relation_reference_matches(qualifier, table) {
                *column = to.to_string();
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests;
