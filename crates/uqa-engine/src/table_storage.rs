//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AnalyzerPhase, Arc, BTreeMap, CommandExactIndex, DocId, Document, Engine, FieldName,
    IVFIndexParams, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult,
    TableState, Value,
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
    uqa_sql::schema::dependencies::walk_schema_expr_mut(expression, &mut |node| {
        visit(node).map_err(|error| error.to_string())
    })
    .map_err(StorageBackendError::Other)
}

pub(crate) fn upgrade_legacy_schema_function_dispatches(
    columns: &mut [uqa_sql::ast::ColumnDef],
    constraints: &mut uqa_sql::ast::TableConstraintSet,
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

pub(crate) use uqa_sql::schema::dependencies::schema_expr_references_column;

pub(crate) fn rename_schema_expr_column(
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
pub(crate) use constraints::{
    foreign_keys_match_without_object_id, materialize_constraint_metadata,
    table_next_id_metadata_key,
};
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
