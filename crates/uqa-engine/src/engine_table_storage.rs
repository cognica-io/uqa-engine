//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AnalyzerPhase, Arc, BTreeMap, CommandExactIndex, CommandMutationOverlay, DocId, Document,
    Engine, FieldName, IVFIndexParams, RelationIdentity, SQLError, StorageBackendError,
    StorageBackendResult, TableState, Value,
};
use crate::CatalogIndexRow;

/// Answer of the value-index conflict probe in [`Engine::find_conflict`].
enum IndexConflictProbe {
    /// No conflict column has a usable value index; fall back to the
    /// evaluated document scan.
    Unanswerable,
    /// The index answered: no existing row matches the conflict target.
    NoConflict,
    /// The index answered: this existing row matches the conflict target.
    Conflict(DocId),
}

fn table_not_found(table: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("table `{table}` does not exist"))
}

fn column_not_found(table: &str, column: &str) -> StorageBackendError {
    StorageBackendError::Other(format!(
        "column `{column}` does not exist on table `{table}`"
    ))
}

fn stored_relation_reference_matches(reference: &str, target: &RelationIdentity) -> bool {
    match RelationIdentity::parse_reference(reference) {
        Ok((Some(schema), name)) => schema == target.schema && name == target.name,
        Ok((None, name)) => name == target.name,
        // Corrupt legacy metadata is never evidence that a dependency is
        // absent. DDL must fail closed rather than leave it dangling.
        Err(_) => true,
    }
}

fn walk_schema_expr_mut(
    expression: &mut uqa_sql::ast::Expr,
    visit: &mut impl FnMut(&mut uqa_sql::ast::Expr) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    use uqa_sql::ast::{Expr, FrameBound};

    visit(expression)?;
    match expression {
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                walk_schema_expr_mut(argument, visit)?;
            }
            for order in order_by {
                walk_schema_expr_mut(&mut order.expr, visit)?;
            }
            if let Some(filter) = filter {
                walk_schema_expr_mut(filter, visit)?;
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                walk_schema_expr_mut(item, visit)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_schema_expr_mut(lhs, visit)?;
            walk_schema_expr_mut(rhs, visit)?;
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            walk_schema_expr_mut(inner, visit)?;
        }
        Expr::Between { expr, low, high } => {
            walk_schema_expr_mut(expr, visit)?;
            walk_schema_expr_mut(low, visit)?;
            walk_schema_expr_mut(high, visit)?;
        }
        Expr::InList { expr, list, .. } => {
            walk_schema_expr_mut(expr, visit)?;
            for item in list {
                walk_schema_expr_mut(item, visit)?;
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args {
                walk_schema_expr_mut(argument, visit)?;
            }
            for partition in &mut spec.partition_by {
                walk_schema_expr_mut(partition, visit)?;
            }
            for order in &mut spec.order_by {
                walk_schema_expr_mut(&mut order.expr, visit)?;
            }
            if let Some(frame) = &mut spec.frame {
                for bound in [&mut frame.start, &mut frame.end] {
                    match bound {
                        FrameBound::Preceding(expression) | FrameBound::Following(expression) => {
                            walk_schema_expr_mut(expression, visit)?;
                        }
                        FrameBound::UnboundedPreceding
                        | FrameBound::UnboundedFollowing
                        | FrameBound::CurrentRow => {}
                    }
                }
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                walk_schema_expr_mut(base, visit)?;
            }
            for (condition, result) in when {
                walk_schema_expr_mut(condition, visit)?;
                walk_schema_expr_mut(result, visit)?;
            }
            if let Some(else_branch) = else_branch {
                walk_schema_expr_mut(else_branch, visit)?;
            }
        }
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            return Err(StorageBackendError::Other(
                "schema expression contains a subquery whose dependencies cannot be rewritten safely"
                    .into(),
            ));
        }
        Expr::Default
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => {}
    }
    Ok(())
}

fn rewrite_sequence_function_references(
    expression: &mut uqa_sql::ast::Expr,
    visit: &mut impl FnMut(&mut String) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    walk_schema_expr_mut(expression, &mut |node| {
        let uqa_sql::ast::Expr::Func { name, args, .. } = node else {
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
            // A dynamically computed text argument deliberately retains
            // late-binding semantics. Only a literal regclass spelling is an
            // early-bound catalog dependency.
            return Ok(());
        };
        visit(reference)
    })
}

fn regclass_literal_mut(expression: &mut uqa_sql::ast::Expr) -> Option<&mut String> {
    match expression {
        uqa_sql::ast::Expr::Literal(Value::Str(reference)) => Some(reference),
        uqa_sql::ast::Expr::Cast { expr, ty }
            if ty.eq_ignore_ascii_case("regclass")
                || ty.eq_ignore_ascii_case("pg_catalog.regclass") =>
        {
            regclass_literal_mut(expr)
        }
        _ => None,
    }
}

pub(crate) fn schema_expr_references_column(expression: &uqa_sql::ast::Expr, column: &str) -> bool {
    let mut expression = expression.clone();
    let mut referenced = false;
    let result = walk_schema_expr_mut(&mut expression, &mut |node| {
        referenced |= match node {
            uqa_sql::ast::Expr::Star | uqa_sql::ast::Expr::QualifiedStar(_) => true,
            uqa_sql::ast::Expr::Column(name)
            | uqa_sql::ast::Expr::QualifiedColumn { column: name, .. } => name == column,
            _ => false,
        };
        Ok(())
    });
    result.is_err() || referenced
}

fn rename_schema_expr_column(
    expression: &mut uqa_sql::ast::Expr,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    walk_schema_expr_mut(expression, &mut |node| {
        match node {
            uqa_sql::ast::Expr::Star | uqa_sql::ast::Expr::QualifiedStar(_) => {
                return Err(StorageBackendError::Other(
                    "schema expression contains `*` and cannot be rewritten safely".into(),
                ));
            }
            uqa_sql::ast::Expr::Column(name) if name == from => *name = to.to_string(),
            uqa_sql::ast::Expr::QualifiedColumn { column, .. } if column == from => {
                *column = to.to_string();
            }
            _ => {}
        }
        Ok(())
    })
}

fn schema_expr_references_relation(
    expression: &uqa_sql::ast::Expr,
    target: &RelationIdentity,
) -> bool {
    let mut expression = expression.clone();
    let mut referenced = false;
    let result = walk_schema_expr_mut(&mut expression, &mut |node| {
        if let uqa_sql::ast::Expr::QualifiedColumn { qualifier, .. } = node {
            referenced |= stored_relation_reference_matches(qualifier, target);
        }
        Ok(())
    });
    result.is_err() || referenced
}

fn rename_schema_expr_relation(
    expression: &mut uqa_sql::ast::Expr,
    from: &RelationIdentity,
    to: &str,
) -> StorageBackendResult<()> {
    walk_schema_expr_mut(expression, &mut |node| {
        if let uqa_sql::ast::Expr::QualifiedColumn { qualifier, .. } = node {
            if stored_relation_reference_matches(qualifier, from) {
                *qualifier = to.to_string();
            }
        }
        Ok(())
    })
}

fn rename_schema_expr_qualified_column(
    expression: &mut uqa_sql::ast::Expr,
    table: &RelationIdentity,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    walk_schema_expr_mut(expression, &mut |node| {
        if let uqa_sql::ast::Expr::QualifiedColumn { qualifier, column } = node {
            if column == from && stored_relation_reference_matches(qualifier, table) {
                *column = to.to_string();
            }
        }
        Ok(())
    })
}

mod columns;
mod constraints;
pub(crate) use constraints::materialize_constraint_names;
mod dependencies;
mod documents;
mod fts;
mod persistent;
mod table_lifecycle;

/// A persistent document-store write failed. Surfacing this as a
/// statement error makes the enclosing transaction roll back, so the
/// on-disk state never keeps a half-applied rewrite.
pub(crate) fn document_store_write_error(err: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("document store write failed: {err}"))
}

pub(crate) fn document_store_read_error(action: &str, err: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("{action} failed: {err}"))
}

#[cfg(test)]
mod tests;
