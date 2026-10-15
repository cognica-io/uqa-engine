//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply retained catalog identities to SQL's expression encoder and renderer.

mod routines;

use uqa_sql::{
    ast::{ColumnDef, Expr},
    catalog::node_tree::{deparse, expressions::ExpressionContext, Field, Node},
    expr::EngineValueCatalog,
    schema::ColumnTypeSchema,
    ColumnType, SQLError,
};

use crate::catalog::{context::CatalogContext, CatalogReadView};

use super::{CatalogOutput, CatalogRequest};

#[cfg(test)]
thread_local! {
    pub(crate) static EXPRESSION_ENCODINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn encode(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    columns: &[ColumnDef],
    domain: Option<&ColumnType>,
    expression: &Expr,
    expected: Option<&ColumnType>,
) -> Result<Node, SQLError> {
    encode_with_schema(
        context,
        catalog,
        &ColumnTypeSchema::new(columns),
        domain,
        expression,
        expected,
    )
}

fn encode_with_schema(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    schema: &ColumnTypeSchema<'_>,
    domain: Option<&ColumnType>,
    expression: &Expr,
    expected: Option<&ColumnType>,
) -> Result<Node, SQLError> {
    #[cfg(test)]
    EXPRESSION_ENCODINGS.set(EXPRESSION_ENCODINGS.get() + 1);
    let context = CatalogContext {
        catalog,
        ..*context
    };
    ExpressionContext {
        schema,
        domain_value: domain,
        types: Some(context.routines),
        routines: &routines::Routines(&context),
    }
    .expression(expression, expected)
}

pub(super) fn constraint_node(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    constraint: &super::helpers::constraints::ConstraintCatalogRow,
    request: &CatalogRequest,
    selected: Option<&crate::catalog::CatalogTableSnapshot>,
) -> Result<Option<Node>, SQLError> {
    let Some(expression) = constraint.expression.as_ref().filter(|_| {
        request.includes("conbin") && request.matches_name("conname", &constraint.name)
    }) else {
        return Ok(None);
    };
    let identity = uqa_core::RelationIdentity::new(&constraint.schema, &constraint.table);
    let (columns, row_type) = match selected {
        Some(table) => (
            table.columns.as_slice(),
            table
                .catalog_oids
                .row_type_reference(&identity)
                .map(ColumnType::Composite)
                .ok_or_else(|| SQLError::Internal("CHECK relation has no row type".into()))?,
        ),
        None => constraint_scope(catalog, constraint)?,
    };
    encode_with_schema(
        context,
        catalog,
        &ColumnTypeSchema::with_relation(columns, &constraint.table, &row_type)?,
        None,
        expression,
        Some(&ColumnType::Boolean),
    )
    .map(Some)
}

pub(super) fn default_node(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    columns: &[ColumnDef],
    expression: &Expr,
    target: &ColumnType,
) -> Result<Node, SQLError> {
    #[cfg(test)]
    EXPRESSION_ENCODINGS.set(EXPRESSION_ENCODINGS.get() + 1);
    let context = CatalogContext {
        catalog,
        ..*context
    };
    ExpressionContext {
        schema: &ColumnTypeSchema::new(columns),
        domain_value: None,
        types: Some(context.routines),
        routines: &routines::Routines(&context),
    }
    .assignment(expression, target)
}

pub(super) fn constraint_text(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    constraint: &super::helpers::constraints::ConstraintCatalogRow,
    pretty: bool,
) -> Result<String, SQLError> {
    let (columns, row_type) = constraint_scope(catalog, constraint)?;
    let expression = constraint
        .expression
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CHECK constraint has no expression".into()))?;
    let node = encode_with_schema(
        context,
        catalog,
        &ColumnTypeSchema::with_relation(columns, &constraint.table, &row_type)?,
        None,
        expression,
        Some(&ColumnType::Boolean),
    )?;
    render_with_relation(
        &CatalogContext {
            catalog,
            ..*context
        },
        columns,
        Some(&constraint.table),
        &node.into(),
        pretty,
    )
}

pub(super) fn domain_node(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    domain: &uqa_sql::catalog::domain::StoredDomain,
    expression: &Expr,
) -> Result<Node, SQLError> {
    let value = ColumnDef::nullable("value", domain.definition.base.clone());
    encode(
        context,
        catalog,
        &[value],
        Some(&domain.definition.base),
        expression,
        Some(&ColumnType::Boolean),
    )
}

fn constraint_scope<'a>(
    catalog: &'a CatalogReadView,
    constraint: &super::helpers::constraints::ConstraintCatalogRow,
) -> Result<(&'a [ColumnDef], ColumnType), SQLError> {
    let identity = uqa_core::RelationIdentity::new(&constraint.schema, &constraint.table);
    let name = identity.qualified_name();
    let (columns, oids) = if let Some(table) = catalog.snapshot().tables.get(&identity) {
        (table.columns.as_slice(), table.catalog_oids)
    } else if let Some(table) = catalog.snapshot().definitions.foreign_tables.get(&identity) {
        (table.columns.as_slice(), table.relation_oids())
    } else {
        return Err(SQLError::UnknownTable(name));
    };
    let row_type = oids
        .row_type_reference(&identity)
        .map(ColumnType::Composite)
        .ok_or_else(|| SQLError::Internal(format!("CHECK relation `{name}` has no row type")))?;
    Ok((columns, row_type))
}

pub(super) fn render(
    context: &CatalogContext<'_>,
    columns: &[ColumnDef],
    value: &Field,
    pretty: bool,
) -> Result<String, SQLError> {
    render_with_relation(context, columns, None, value, pretty)
}

pub(super) fn render_with_relation(
    context: &CatalogContext<'_>,
    columns: &[ColumnDef],
    relation: Option<&str>,
    value: &Field,
    pretty: bool,
) -> Result<String, SQLError> {
    let (_, setting) = context.session.show_parameter("IntervalStyle")?;
    let style = uqa_sql::semantics::parameters::intervalstyle::style(&setting)
        .ok_or_else(|| SQLError::Internal("invalid session IntervalStyle".into()))?;
    let _interval_style = uqa_sql::expr::IntervalStyleScope::enter(style);
    let output = CatalogOutput(*context);
    let values = EngineValueCatalog(&output);
    deparse::expression(
        value,
        &Names {
            context,
            columns,
            relation,
            values: &values,
        },
        pretty,
    )
}

struct Names<'a, 'b> {
    context: &'a CatalogContext<'b>,
    columns: &'a [ColumnDef],
    relation: Option<&'a str>,
    values: &'a dyn uqa_sql::expr::SQLValueCatalog,
}

impl deparse::ExpressionNames for Names<'_, '_> {
    fn relation_name(&self) -> Result<String, SQLError> {
        self.relation.map(str::to_owned).ok_or_else(|| {
            SQLError::Internal("whole-row reference requires its relation name".into())
        })
    }
    fn column(&self, attribute: i64) -> Result<String, SQLError> {
        self.columns
            .iter()
            .enumerate()
            .find_map(|(position, column)| {
                let ordinal = column
                    .attribute_number
                    .map_or(position as i64 + 1, i64::from);
                (ordinal == attribute).then(|| column.name.clone())
            })
            .ok_or_else(|| SQLError::Internal(format!("invalid attnum {attribute} for relation")))
    }

    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        super::regtypes::routine_name_parts(self.context, oid)?
            .ok_or_else(|| SQLError::Internal(format!("unknown catalog routine OID {oid}")))
    }

    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        match super::format_type_value(
            self.context,
            &[uqa_core::Value::Int(oid), uqa_core::Value::Int(modifier)],
        )? {
            uqa_core::Value::Str(name) => Ok(name),
            _ => Err(SQLError::Internal(format!(
                "unknown catalog type OID {oid}"
            ))),
        }
    }

    fn value_catalog(&self) -> Option<&dyn uqa_sql::expr::SQLValueCatalog> {
        Some(self.values)
    }

    fn standard_conforming_strings(&self) -> Result<bool, SQLError> {
        self.context
            .session
            .show_parameter("standard_conforming_strings")
            .map(|(_, setting)| setting == "on")
    }
}
