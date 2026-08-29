//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text-index field discovery and pre-execution validation.

use super::{
    multi_field_match_shape, Engine, MultiFieldMatchShape, SQLError, ScalarExpr, SourcePlan, Value,
};

const SINGLE_FIELD_TEXT_MATCH_FUNCTIONS: [&str; 4] = [
    "text_match",
    "bayesian_match",
    "fts_match",
    "bayesian_match_with_prior",
];

/// Walk an expression tree and hand every text-match field argument to
/// `validate`. Used by the select runners to reject silently-empty
/// searches before the WHERE reaches either the operator-tree access path
/// or scalar evaluation in the relational filter node.
fn walk_text_match_fields(
    expr: &ScalarExpr,
    validate: &mut dyn FnMut(&ScalarExpr, &str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    match expr {
        ScalarExpr::Func {
            name, args, filter, ..
        } => {
            let lower = name.to_ascii_lowercase();
            if SINGLE_FIELD_TEXT_MATCH_FUNCTIONS.contains(&lower.as_str()) {
                if let Some(field_arg) = args.first() {
                    if !(lower == "fts_match" && fts_query_is_jsonpath(args.get(1))) {
                        validate(field_arg, &lower)?;
                    }
                }
            } else if lower == "multi_field_match" {
                match multi_field_match_shape(args)? {
                    MultiFieldMatchShape::FieldsThenQuery { fields, .. }
                    | MultiFieldMatchShape::Pairs { fields } => {
                        for field_arg in fields {
                            validate(field_arg, "multi_field_match")?;
                        }
                    }
                }
            }
            for arg in args {
                walk_text_match_fields(arg, validate)?;
            }
            if let Some(filter) = filter {
                walk_text_match_fields(filter, validate)?;
            }
            Ok(())
        }
        ScalarExpr::And(items)
        | ScalarExpr::Or(items)
        | ScalarExpr::Array(items)
        | ScalarExpr::Row(items) => {
            for item in items {
                walk_text_match_fields(item, validate)?;
            }
            Ok(())
        }
        ScalarExpr::Not(inner) | ScalarExpr::UnaryMinus(inner) => {
            walk_text_match_fields(inner, validate)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            walk_text_match_fields(lhs, validate)?;
            walk_text_match_fields(rhs, validate)
        }
        ScalarExpr::IsNull { expr, .. } => walk_text_match_fields(expr, validate),
        ScalarExpr::Between { expr, low, high } => {
            walk_text_match_fields(expr, validate)?;
            walk_text_match_fields(low, validate)?;
            walk_text_match_fields(high, validate)
        }
        ScalarExpr::InList { expr, list, .. } => {
            walk_text_match_fields(expr, validate)?;
            for item in list {
                walk_text_match_fields(item, validate)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(in crate::sql) fn validate_expr_text_match_fields(
    engine: &Engine,
    table: &str,
    expr: &ScalarExpr,
) -> Result<(), SQLError> {
    walk_text_match_fields(
        expr,
        &mut |field_arg, function_name| match text_match_field_name(field_arg) {
            Some(TextMatchField::All) => {
                validate_text_match_all_fields(engine, table, function_name)
            }
            Some(TextMatchField::Named(field)) => {
                validate_text_match_field(engine, table, field, function_name)
            }
            None => Ok(()),
        },
    )
}

enum TextMatchField<'a> {
    All,
    Named(&'a str),
}

/// The `_all` pseudo-field arrives either as a string literal or as a
/// bare column reference, depending on how the query was written.
fn text_match_field_name(field_arg: &ScalarExpr) -> Option<TextMatchField<'_>> {
    match field_arg {
        ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. } => {
            if name.is_empty() || name == "_all" {
                Some(TextMatchField::All)
            } else {
                Some(TextMatchField::Named(name))
            }
        }
        ScalarExpr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => {
            Some(TextMatchField::All)
        }
        _ => None,
    }
}

pub(in crate::sql) fn validate_joined_expr_text_match_fields(
    engine: &Engine,
    from: &SourcePlan,
    expr: &ScalarExpr,
) -> Result<(), SQLError> {
    let mut tables: Vec<(Option<String>, String)> = Vec::new();
    let mut has_opaque_source = false;
    collect_from_tables(from, &mut tables, &mut has_opaque_source);
    walk_text_match_fields(expr, &mut |field_arg, function_name| {
        let (qualifier, column) = match field_arg {
            ScalarExpr::Column(name) => (None, name.as_str()),
            ScalarExpr::QualifiedColumn {
                qualifier, column, ..
            } => (Some(qualifier.as_str()), column.as_str()),
            _ => return Ok(()),
        };
        if column.is_empty() || column == "_all" {
            return Ok(());
        }
        if let Some(qualifier) = qualifier {
            let resolved = tables
                .iter()
                .find(|(alias, name)| alias.as_deref() == Some(qualifier) || name == qualifier);
            return match resolved {
                Some((_, table)) => validate_text_match_field(engine, table, column, function_name),
                // Unknown qualifiers can point at subqueries or CTEs the
                // validator cannot introspect.
                None => Ok(()),
            };
        }
        let mut containing: Vec<&String> = Vec::new();
        for (_, name) in &tables {
            if engine
                .try_query_table_has_column(name, column)
                .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?
            {
                containing.push(name);
            }
        }
        for name in &containing {
            if engine
                .fts_fields_for_table(name)?
                .iter()
                .any(|f| f == column)
            {
                return Ok(());
            }
        }
        if let Some(table) = containing.first() {
            return validate_text_match_field(engine, table, column, function_name);
        }
        if has_opaque_source {
            return Ok(());
        }
        Err(SQLError::TypeMismatch(format!(
            "{function_name}: column `{column}` does not exist on any joined table"
        )))
    })
}

/// The `@@` operator doubles as a `JSONPath` match when the right-hand
/// side is a `$...` path literal; that form evaluates row-level JSON and
/// needs no text index.
fn fts_query_is_jsonpath(query_arg: Option<&ScalarExpr>) -> bool {
    matches!(
        query_arg,
        Some(ScalarExpr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')
    )
}

fn collect_from_tables(
    from: &SourcePlan,
    out: &mut Vec<(Option<String>, String)>,
    has_opaque_source: &mut bool,
) {
    match from {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } => out.push((
            Some(alias.as_ref().unwrap_or(qualifier).clone()),
            name.clone(),
        )),
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if alias.is_some() {
                *has_opaque_source = true;
            } else {
                collect_from_tables(left, out, has_opaque_source);
                collect_from_tables(right, out, has_opaque_source);
            }
        }
        _ => *has_opaque_source = true,
    }
}

/// Reject silently-empty text searches up front: a match function whose
/// field is not a real column, or is a column without a text index,
/// previously returned zero rows with no diagnostic.
pub(in crate::sql) fn validate_text_match_field(
    engine: &Engine,
    table: &str,
    field: &str,
    function_name: &str,
) -> Result<(), SQLError> {
    if !engine
        .try_query_has_table(table)
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: unknown table `{table}`"
        )));
    }
    let indexed = engine
        .fts_fields_for_table(table)?
        .iter()
        .any(|fts| fts == field);
    if !indexed {
        if !engine
            .try_query_table_has_column(table, field)
            .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?
            && !engine
                .try_query_table_columns(table)
                .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?
                .is_empty()
        {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name}: column `{field}` does not exist on table `{table}`"
            )));
        }
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: column `{table}.{field}` has no text index; \
             create one with CREATE INDEX ... ON {table} USING gin ({field})"
        )));
    }
    Ok(())
}

pub(in crate::sql) fn validate_text_match_all_fields(
    engine: &Engine,
    table: &str,
    function_name: &str,
) -> Result<(), SQLError> {
    if !engine
        .try_query_has_table(table)
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: unknown table `{table}`"
        )));
    }
    if engine.fts_fields_for_table(table)?.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}: table `{table}` has no text-indexed columns; \
             create one with CREATE INDEX ... ON {table} USING gin (...)"
        )));
    }
    Ok(())
}
