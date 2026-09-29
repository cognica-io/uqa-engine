//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-aware reconstruction of durable view queries.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ir::ScalarExpr;
use uqa_sql::plan::QueryPlan;
use uqa_sql::{expr::quote_ident, SQLError};

use crate::catalog::{
    context::CatalogContext,
    view::{StoredView, StoredViewKind},
};
use crate::catalog::{
    CatalogReadView, RelationLookupMode, RelationNameResolution, RelationResolution,
};
use uqa_core::RelationIdentity;

mod expressions;
mod query;
mod rename;
mod subscripts;
mod types;
pub use rename::{rename_view_column_query, view_query_references_column};
mod sources;

pub fn pg_get_viewdef_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    if !(1..=2).contains(&arguments.len()) {
        return Err(SQLError::BadArity {
            name: "pg_get_viewdef".into(),
            expected: "1 or 2".into(),
            actual: arguments.len(),
        });
    }
    if arguments.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(Value::Null);
    }
    let (pretty, wrap) = match arguments.get(1) {
        None => (false, 0),
        Some(Value::Bool(pretty)) => (*pretty, 0),
        Some(Value::Int(wrap)) => (true, *wrap),
        Some(_) => {
            return Err(SQLError::TypeMismatch(
                "invalid pg_get_viewdef option".into(),
            ))
        }
    };
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    let view = match &arguments[0] {
        Value::Int(oid) => [StoredViewKind::View, StoredViewKind::Materialized]
            .into_iter()
            .flat_map(|kind| catalog.views_of_kind(kind))
            .find_map(|(_, view)| (super::view_relation_oid(&view) == *oid).then_some(view)),
        Value::Str(name) | Value::FixedChar(name) => {
            let reference = view_name_reference(name)?;
            if catalog
                .virtual_relation_resolved(&resolution, &reference)?
                .is_some()
            {
                return Ok(Value::Null);
            }
            let canonical = match catalog.relation_kind_resolution(&resolution, &reference)? {
                RelationResolution::Found(canonical, _) => canonical,
                RelationResolution::MissingSchema(schema) => {
                    return Err(SQLError::Routine {
                        sqlstate: "3F000".into(),
                        message: format!("schema \"{schema}\" does not exist"),
                    })
                }
                RelationResolution::MissingRelation => {
                    return Err(SQLError::UnknownTable(name.clone()))
                }
            };
            let mut bound = resolution.clone();
            bound.set_lookup_mode(RelationLookupMode::Bound);
            catalog.view_resolved(&bound, &canonical)?.cloned()
        }
        _ => {
            return Err(SQLError::TypeMismatch(
                "pg_get_viewdef requires text or oid".into(),
            ))
        }
    };
    view.map_or(Ok(Value::Null), |view| {
        view_definition(&catalog, &resolution, &view, pretty, wrap).map(Value::Str)
    })
}

fn view_name_reference(name: &str) -> Result<String, SQLError> {
    let names = uqa_sql::parse_regobject_name(name).ok_or_else(|| SQLError::Routine {
        sqlstate: "42602".into(),
        message: "invalid name syntax".into(),
    })?;
    let names = match names.as_slice() {
        [_, _] | [_] => names.as_slice(),
        [database, schema, relation] if database == "uqa" => {
            return Ok(format!("{}.{}", quote_ident(schema), quote_ident(relation)));
        }
        [_, _, _] => {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("cross-database references are not implemented: {name}"),
            });
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: format!("improper qualified name (too many dotted names): {name}"),
            });
        }
    };
    Ok(names
        .iter()
        .map(|name| quote_ident(name))
        .collect::<Vec<_>>()
        .join("."))
}

pub fn view_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    view: &StoredView,
    pretty: bool,
    wrap: i64,
) -> Result<String, SQLError> {
    let mut dynamic = resolution.clone();
    dynamic.set_lookup_mode(RelationLookupMode::Dynamic);
    let mut bound = resolution.clone();
    bound.set_lookup_mode(RelationLookupMode::Bound);
    let deparser = Deparser {
        catalog,
        dynamic,
        bound,
        pretty,
        wrap,
        standalone: false,
    };
    let mut rendered = deparser.query(
        &view.query,
        &Scope::default(),
        view.output_columns.as_deref(),
    )?;
    rendered.push(';');
    Ok(rendered)
}

struct Deparser<'a> {
    catalog: &'a CatalogReadView,
    dynamic: RelationNameResolution,
    bound: RelationNameResolution,
    pretty: bool,
    wrap: i64,
    /// A standalone expression, as `pg_get_expr` and `pg_get_indexdef` print, rather than a clause of a query.
    standalone: bool,
}

#[derive(Clone)]
struct Column {
    name: String,
    qualifier: String,
    rendered_qualifier: String,
    merged: Option<String>,
    relation: Option<String>,
    merged_expression: Option<ScalarExpr>,
}

#[derive(Clone, Default)]
struct Scope {
    columns: Vec<Column>,
    outer: Vec<Column>,
    ctes: BTreeMap<String, Vec<String>>,
    indent: usize,
    nested: bool,
    qualify: bool,
}

impl Scope {
    fn child(&self) -> Self {
        Self {
            outer: self.columns.iter().chain(&self.outer).cloned().collect(),
            ctes: self.ctes.clone(),
            indent: self.indent + 8,
            nested: true,
            ..Self::default()
        }
    }

    fn column(&self, qualifier: Option<&str>, name: &str) -> String {
        let matches = |column: &&Column| {
            column.name == name && qualifier.is_none_or(|qualifier| column.qualifier == qualifier)
        };
        if let Some(column) = self.columns.iter().find(matches) {
            if qualifier.is_none() {
                if let Some(merged) = &column.merged {
                    return merged.clone();
                }
            }
            return render_column(column, self.qualify);
        }
        if let Some(column) = self.outer.iter().find(matches) {
            return render_column(column, true);
        }
        qualifier.map_or_else(
            || quote_ident(name),
            |qualifier| format!("{}.{}", quote_ident(qualifier), quote_ident(name)),
        )
    }
}

fn render_column(column: &Column, qualified: bool) -> String {
    if qualified && !column.rendered_qualifier.is_empty() {
        format!(
            "{}.{}",
            quote_ident(&column.rendered_qualifier),
            quote_ident(&column.name)
        )
    } else {
        quote_ident(&column.name)
    }
}

fn expression_name(expression: &ScalarExpr) -> String {
    match expression {
        ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. } => name.clone(),
        ScalarExpr::Func { name, .. } | ScalarExpr::WindowCall { name, .. } => {
            name.rsplit('.').next().unwrap_or(name).to_string()
        }
        ScalarExpr::Cast { expr, ty } => {
            let name = expression_name(expr);
            if name == "?column?" {
                ty.clone()
            } else {
                name
            }
        }
        ScalarExpr::Case { .. } => "case".into(),
        ScalarExpr::Array(_) => "array".into(),
        ScalarExpr::Row(_) => "row".into(),
        ScalarExpr::Exists { .. } => "exists".into(),
        _ => "?column?".into(),
    }
}

fn query_columns(query: &QueryPlan) -> Vec<String> {
    match &query.root {
        uqa_sql::plan::RelationalPlan::QueryBlock(block) => block
            .projections
            .iter()
            .map(|projection| {
                projection
                    .alias
                    .clone()
                    .unwrap_or_else(|| expression_name(&projection.expr))
            })
            .collect(),
        uqa_sql::plan::RelationalPlan::SetOp { left, .. } => query_columns(left),
        uqa_sql::plan::RelationalPlan::Values { rows, .. } => (0..rows.first().map_or(0, Vec::len))
            .map(|index| format!("column{}", index + 1))
            .collect(),
    }
}

/// Reconstruct a stored catalog expression with the same literal casts, precedence, and routine visibility as a view definition.
pub fn stored_expression_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    expression: &uqa_sql::ast::Expr,
    pretty: bool,
) -> Result<String, SQLError> {
    let mut dynamic = resolution.clone();
    dynamic.set_lookup_mode(RelationLookupMode::Dynamic);
    let mut bound = resolution.clone();
    bound.set_lookup_mode(RelationLookupMode::Bound);
    let deparser = Deparser {
        catalog,
        dynamic,
        bound,
        pretty,
        wrap: 0,
        standalone: true,
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(expression.clone());
    deparser.expression(
        &expression.scalar,
        &Scope::default(),
        &expression.subqueries,
    )
}

/// A domain constraint expression, whose value placeholder prints as `VALUE`.
pub fn stored_domain_expression_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    expression: &uqa_sql::ast::Expr,
    pretty: bool,
) -> Result<String, SQLError> {
    let mut dynamic = resolution.clone();
    dynamic.set_lookup_mode(RelationLookupMode::Dynamic);
    let mut bound = resolution.clone();
    bound.set_lookup_mode(RelationLookupMode::Bound);
    let deparser = Deparser {
        catalog,
        dynamic,
        bound,
        pretty,
        wrap: 0,
        standalone: true,
    };
    let scope = Scope {
        columns: vec![Column {
            name: "value".into(),
            qualifier: String::new(),
            rendered_qualifier: String::new(),
            merged: Some("VALUE".into()),
            relation: None,
            merged_expression: None,
        }],
        ..Scope::default()
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(expression.clone());
    deparser.expression(&expression.scalar, &scope, &expression.subqueries)
}

/// One statement of a `BEGIN ATOMIC` body, as `get_query_def` prints a query with indentation and full parenthesization.
pub fn stored_statement_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    statement: &uqa_sql::ast::Statement,
) -> Result<String, SQLError> {
    let mut dynamic = resolution.clone();
    dynamic.set_lookup_mode(RelationLookupMode::Dynamic);
    let mut bound = resolution.clone();
    bound.set_lookup_mode(RelationLookupMode::Bound);
    let deparser = Deparser {
        catalog,
        dynamic,
        bound,
        pretty: false,
        wrap: 0,
        standalone: false,
    };
    match statement {
        uqa_sql::ast::Statement::Select(select) => {
            let query = uqa_sql::plan::QueryPlan::lower((**select).clone());
            deparser.query(&query, &Scope::default(), None)
        }
        uqa_sql::ast::Statement::Insert(_) => Err(unsupported_body_statement("INSERT")),
        uqa_sql::ast::Statement::Update(_) => Err(unsupported_body_statement("UPDATE")),
        uqa_sql::ast::Statement::Delete(_) => Err(unsupported_body_statement("DELETE")),
        uqa_sql::ast::Statement::Merge(_) => Err(unsupported_body_statement("MERGE")),
        _ => Err(unsupported_body_statement("utility")),
    }
}

fn unsupported_body_statement(kind: &str) -> SQLError {
    SQLError::Unsupported(format!(
        "pg_get_function_sqlbody cannot print a {kind} statement"
    ))
}

/// A stored catalog expression as `pg_get_expr` prints it without pretty-printing.
pub fn stored_expression_text(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    expression: &uqa_sql::ast::Expr,
) -> Result<String, SQLError> {
    stored_expression_definition(catalog, resolution, expression, false)
}
