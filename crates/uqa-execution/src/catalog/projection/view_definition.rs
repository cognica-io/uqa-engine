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
mod fields;
mod query;
mod references;
mod rename;
mod routine_body;
mod statements;
mod subscripts;
mod types;
mod windows;
pub use references::query_references;
pub use rename::{rename_view_column_query, view_query_references_column};
pub use routine_body::RoutineNamespace;
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
        view_definition(
            Some(&crate::catalog::projection::CatalogOutput(*context)),
            &catalog,
            &resolution,
            &view,
            pretty,
            wrap,
        )
        .map(Value::Str)
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
    output: Option<&dyn uqa_sql::expr::EngineHook>,
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
        output,
        catalog,
        dynamic,
        bound,
        pretty,
        wrap,
        standalone: false,
        indent: true,
        routine: None,
        aliases: std::cell::OnceCell::new(),
    };
    let mut rendered = deparser.query(
        &view.query,
        &Scope {
            column_names_visible: true,
            ..Scope::default()
        },
        view.output_columns.as_deref(),
    )?;
    rendered.push(';');
    Ok(rendered)
}

struct Deparser<'a> {
    output: Option<&'a dyn uqa_sql::expr::EngineHook>,
    catalog: &'a CatalogReadView,
    dynamic: RelationNameResolution,
    bound: RelationNameResolution,
    pretty: bool,
    wrap: i64,
    /// A standalone expression, as `pg_get_expr` and `pg_get_indexdef` print, rather than a clause of a query.
    standalone: bool,
    /// `PRETTYFLAG_INDENT`: clauses start new lines and nested queries indent. Only `RETURN` bodies print without it.
    indent: bool,
    /// The routine whose SQL-standard body is printed, whose parameters print by name.
    routine: Option<RoutineNamespace>,
    /// The output of `regtype`, `regproc`, `regprocedure` and `regnamespace` constants, built when the first one is printed.
    aliases: std::cell::OnceCell<crate::catalog::projection::regtypes::AliasConstantOutput>,
}

#[derive(Clone)]
struct Column {
    name: String,
    qualifier: String,
    rendered_qualifier: String,
    merged: Option<String>,
    relation: Option<String>,
    merged_expression: Option<ScalarExpr>,
    /// The relation column this column reads, by relation name and column name, whatever alias names it.
    base: Option<(String, String)>,
    /// A join input's own copy of a `USING` column, which qualified references reach but `*` does not.
    hidden: bool,
}

#[derive(Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate field of ruleutils.c's deparse context"
)]
struct Scope {
    columns: Vec<Column>,
    outer: Vec<Column>,
    ctes: BTreeMap<String, Vec<String>>,
    windows: Vec<windows::RenderedWindow>,
    indent: usize,
    nested: bool,
    qualify: bool,
    /// This query level or one enclosing it has a range table, so a routine parameter prints qualified by the routine's name.
    range_table: bool,
    /// `colNamesVisible`: the output column names of this query level matter, so every column that is not a plain column reference prints its name.
    column_names_visible: bool,
    /// The query is an `INSERT`'s source, whose output literals `transformInsertStmt` leaves `unknown` for the target columns to coerce, so they print without a type.
    unknown_outputs: bool,
}

impl Scope {
    /// The scope of a subquery nested in this one, whose output column names do not matter.
    fn child(&self) -> Self {
        Self {
            outer: self.columns.iter().chain(&self.outer).cloned().collect(),
            ctes: self.ctes.clone(),
            indent: self.indent + 8,
            nested: true,
            range_table: self.range_table,
            ..Self::default()
        }
    }

    /// The scope of a subquery whose output column names matter: a `WITH` query or a subquery in `FROM`.
    fn named_child(&self) -> Self {
        Self {
            column_names_visible: true,
            ..self.child()
        }
    }

    /// Whether a relation of this query level or an enclosing one has this alias.
    fn has_relation(&self, qualifier: &str) -> bool {
        self.columns
            .iter()
            .chain(&self.outer)
            .any(|column| column.qualifier == qualifier)
    }

    /// Whether a column reference names a column of this query level or an enclosing one.
    fn resolves(&self, qualifier: Option<&str>, name: &str) -> bool {
        self.columns.iter().chain(&self.outer).any(|column| {
            column.name == name && qualifier.is_none_or(|qualifier| column.qualifier == qualifier)
        })
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
        ScalarExpr::Func { name, binding, .. } => {
            uqa_sql::semantics::function_projection_label(name, binding.as_ref())
        }
        ScalarExpr::WindowCall { name, .. } => {
            uqa_sql::semantics::function_projection_label(name, None)
        }
        ScalarExpr::Cast { expr, ty, .. } => {
            let name = expression_name(expr);
            if name == "?column?" {
                ty.clone()
            } else {
                name
            }
        }
        ScalarExpr::Case { .. } => "case".into(),
        ScalarExpr::Array(_) => "array".into(),
        ScalarExpr::Row(_) | ScalarExpr::CompositeRow { .. } => "row".into(),
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
    output: Option<&dyn uqa_sql::expr::EngineHook>,
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
        output,
        catalog,
        dynamic,
        bound,
        pretty,
        wrap: 0,
        standalone: true,
        indent: true,
        routine: None,
        aliases: std::cell::OnceCell::new(),
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
    output: Option<&dyn uqa_sql::expr::EngineHook>,
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
        output,
        catalog,
        dynamic,
        bound,
        pretty,
        wrap: 0,
        standalone: true,
        indent: true,
        routine: None,
        aliases: std::cell::OnceCell::new(),
    };
    let scope = Scope {
        columns: vec![Column {
            name: "value".into(),
            qualifier: String::new(),
            rendered_qualifier: String::new(),
            merged: Some("VALUE".into()),
            relation: None,
            merged_expression: None,
            base: None,
            hidden: false,
        }],
        ..Scope::default()
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(expression.clone());
    deparser.expression(&expression.scalar, &scope, &expression.subqueries)
}

/// A routine's SQL-standard body as `print_function_sqlbody` prints it: a `RETURN` body's expression without indentation, or each statement of a `BEGIN ATOMIC` body at indentation level one, with the routine's parameters named.
pub fn routine_body_definition(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    def: &uqa_sql::ast::CreateFunction,
    form: uqa_sql::ast::SQLBodyForm,
    statements: &[uqa_sql::ast::Statement],
) -> Result<String, SQLError> {
    let mut dynamic = resolution.clone();
    dynamic.set_lookup_mode(RelationLookupMode::Dynamic);
    let mut bound = resolution.clone();
    bound.set_lookup_mode(RelationLookupMode::Bound);
    let atomic = form == uqa_sql::ast::SQLBodyForm::Atomic;
    let deparser = Deparser {
        output,
        catalog,
        dynamic,
        bound,
        pretty: false,
        wrap: 0,
        standalone: false,
        indent: atomic,
        routine: Some(RoutineNamespace::new(def)?),
        aliases: std::cell::OnceCell::new(),
    };
    // The body's queries sit below the routine's namespace, so their column references always carry a relation name.
    let scope = Scope {
        indent: usize::from(atomic),
        nested: true,
        ..Scope::default()
    };
    if !atomic {
        let [uqa_sql::ast::Statement::Select(select)] = statements else {
            return Err(SQLError::Internal(format!(
                "RETURN body of `{}` is not one SELECT",
                def.name
            )));
        };
        let [projection] = select.projections.as_slice() else {
            return Err(SQLError::Internal(format!(
                "RETURN body of `{}` selects more than one value",
                def.name
            )));
        };
        let expression = uqa_sql::plan::ExpressionPlan::lower(projection.expr.clone());
        return Ok(format!(
            "RETURN {}",
            deparser.expression(&expression.scalar, &scope, &expression.subqueries)?
        ));
    }
    let mut body = String::from("BEGIN ATOMIC\n");
    for statement in statements {
        let mut statement = statement.clone();
        uqa_sql::routines::merge_columns::render_stored_merge_target_columns(
            &routine_body::StoredMergeColumns {
                catalog,
                resolution: &deparser.bound,
            },
            &mut statement,
        )?;
        let plan = uqa_sql::plan::UnifiedPlan::lower(statement);
        body.push_str(&deparser.statement(&plan, &scope)?);
        body.push_str(";\n");
    }
    body.push_str("END");
    Ok(body)
}

/// A trigger's `WHEN` condition as `pg_get_triggerdef` prints it: `get_rule_expr` over the trigger relation's `old` and `new` entries at the standard indentation, with constants spelled through the catalog.
pub fn trigger_condition_definition(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    condition: &uqa_sql::ast::Expr,
    pretty: bool,
) -> Result<String, SQLError> {
    let expression = uqa_sql::plan::ExpressionPlan::lower(condition.clone());
    event_deparser(output, catalog, resolution, pretty).expression(
        &expression.scalar,
        &Scope::default(),
        &expression.subqueries,
    )
}

/// A rewrite rule as `make_ruledef` prints it, which always indents: the event and the relation, which `relation` names as the caller qualified it, the condition over the rule's `old` and `new` entries, and each action as `get_query_def` prints it, followed by a semicolon.
pub fn rule_definition(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    definition: &uqa_sql::ast::CreateRule,
    relation: &str,
    pretty: bool,
) -> Result<String, SQLError> {
    let deparser = event_deparser(output, catalog, resolution, pretty);
    let event = match definition.event {
        uqa_sql::ast::RuleEvent::Select => "SELECT",
        uqa_sql::ast::RuleEvent::Update => "UPDATE",
        uqa_sql::ast::RuleEvent::Insert => "INSERT",
        uqa_sql::ast::RuleEvent::Delete => "DELETE",
    };
    let mut rendered = format!(
        "CREATE RULE {} AS\n    ON {event} TO {relation}",
        quote_ident(&definition.name)
    );
    if let Some(condition) = &definition.condition {
        let expression = uqa_sql::plan::ExpressionPlan::lower(condition.clone());
        rendered.push_str("\n   WHERE ");
        append_context_text(
            &mut rendered,
            &deparser.expression(
                &expression.scalar,
                &Scope::default(),
                &expression.subqueries,
            )?,
        );
    }
    rendered.push_str(" DO ");
    if definition.instead {
        rendered.push_str("INSTEAD ");
    }
    // The actions' range tables hold the rule's `old` and `new` entries, so their column references always carry a relation name.
    let scope = Scope {
        nested: true,
        column_names_visible: true,
        ..Scope::default()
    };
    let action = |statement: &uqa_sql::ast::Statement| {
        deparser.statement(
            &uqa_sql::plan::UnifiedPlan::lower(statement.clone()),
            &scope,
        )
    };
    match definition.actions.as_slice() {
        [] => rendered.push_str("NOTHING;"),
        [statement] => {
            append_context_text(&mut rendered, &action(statement)?);
            rendered.push(';');
        }
        statements => {
            rendered.push('(');
            for statement in statements {
                append_context_text(&mut rendered, &action(statement)?);
                rendered.push_str(";\n");
            }
            rendered.push_str(");");
        }
    }
    Ok(rendered)
}

/// Append deparsed text that may start with `appendContextKeyword`'s line break, which first removes the spaces the buffer ends with.
fn append_context_text(rendered: &mut String, text: &str) {
    if text.starts_with('\n') {
        rendered.truncate(rendered.trim_end_matches(' ').len());
    }
    rendered.push_str(text);
}

/// The deparser of trigger conditions and rewrite rules, which `ruleutils.c` prints with `PRETTYFLAG_INDENT` whether or not the caller asked for pretty output.
fn event_deparser<'a>(
    output: Option<&'a dyn uqa_sql::expr::EngineHook>,
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
    pretty: bool,
) -> Deparser<'a> {
    let mut dynamic = resolution.clone();
    dynamic.set_lookup_mode(RelationLookupMode::Dynamic);
    let mut bound = resolution.clone();
    bound.set_lookup_mode(RelationLookupMode::Bound);
    Deparser {
        output,
        catalog,
        dynamic,
        bound,
        pretty,
        wrap: 0,
        standalone: false,
        indent: true,
        routine: None,
        aliases: std::cell::OnceCell::new(),
    }
}

/// A stored catalog expression as `pg_get_expr` prints it without pretty-printing.
pub fn stored_expression_text(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    expression: &uqa_sql::ast::Expr,
) -> Result<String, SQLError> {
    stored_expression_definition(output, catalog, resolution, expression, false)
}

/// Whether a stored expression prints as a function call, which needs no parentheses of its own where `PostgreSQL` prints an expression in an index or partition key (`looks_like_function`).
pub fn stored_expression_prints_as_call(expression: &uqa_sql::ast::Expr) -> bool {
    let expression = uqa_sql::plan::ExpressionPlan::lower(expression.clone());
    matches!(
        &expression.scalar,
        ScalarExpr::Func { name, binding, args, .. }
            if expressions::prints_as_call(name, binding.as_ref(), args)
    )
}
