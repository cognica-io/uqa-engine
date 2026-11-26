//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed index-key preparation and column names assigned at index creation.

use crate::ast::{Expr, GeneratedColumnKind, IndexKey};
use crate::catalog::index::operator_classes::require_default_btree_operator_class;
use crate::schema::SchemaBindingContext;
use crate::{ast::CreateIndex, ColumnType, SQLError};

pub fn key_names(keys: &[IndexKey]) -> Vec<String> {
    unique_attribute_names(keys.iter().map(|key| match key {
        IndexKey::Column(column) => column.clone(),
        IndexKey::Expression(expression) => {
            expression_name(expression).map_or_else(|| "expr".into(), |(name, _)| name)
        }
    }))
}

pub(crate) fn unique_attribute_names(labels: impl Iterator<Item = String>) -> Vec<String> {
    let mut names = Vec::with_capacity(labels.size_hint().0);
    for label in labels {
        let mut name = label.clone();
        let mut suffix = 1_u64;
        while names.contains(&name) {
            name = format!("{label}{suffix}");
            suffix += 1;
        }
        names.push(name);
    }
    names
}

fn expression_name(expression: &Expr) -> Option<(String, bool)> {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
            Some((name.to_string(), true))
        }
        Expr::Func {
            binding: Some(binding),
            args,
            ..
        } if binding.dispatch == Some(crate::ast::FunctionDispatch::FieldSelect) => {
            match args.as_slice() {
                [_, Expr::Literal(uqa_core::Value::Str(name))] => Some((name.clone(), true)),
                _ => None,
            }
        }
        Expr::Func { name, .. } => Some((
            crate::parse_regobject_name(name)
                .and_then(|mut names| names.pop())
                .unwrap_or_else(|| name.clone()),
            true,
        )),
        Expr::Cast { expr, ty, .. } => {
            let inner = expression_name(expr);
            if inner.as_ref().is_some_and(|(_, strong)| *strong) {
                inner
            } else {
                Some((
                    crate::parse_regtype_name(ty)
                        .ok()
                        .flatten()
                        .and_then(|mut name| name.names.pop())
                        .unwrap_or_else(|| ty.clone()),
                    false,
                ))
            }
        }
        Expr::Case { else_branch, .. } => {
            let inner = else_branch.as_deref().and_then(expression_name);
            Some(
                inner
                    .filter(|(_, strong)| *strong)
                    .unwrap_or_else(|| ("case".into(), false)),
            )
        }
        Expr::Array(_) => Some(("array".into(), true)),
        Expr::Row(_) | Expr::CompositeRow { .. } => Some(("row".into(), true)),
        _ => None,
    }
}

pub fn require_column_key<'a>(key: &'a IndexKey, method: &str) -> Result<&'a str, SQLError> {
    key.column().ok_or_else(|| {
        SQLError::Unsupported(format!(
            "expression keys for access method `{method}` are not implemented"
        ))
    })
}

pub fn prepare_index_keys(
    context: &SchemaBindingContext<'_, '_>,
    statement: &mut CreateIndex,
) -> Result<Vec<ColumnType>, SQLError> {
    let expressions = bind_index_keys(context, statement)?;
    finish_index_keys(context, statement, &expressions)
}

fn bind_index_keys(
    context: &SchemaBindingContext<'_, '_>,
    statement: &mut CreateIndex,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    statement
        .columns
        .iter_mut()
        .map(|key| {
            let IndexKey::Expression(expression) = key else {
                return Ok(None);
            };
            let ty = super::bind_index_expression(
                context.catalog,
                context.binding,
                &statement.table,
                expression,
                false,
            )?;
            if let Some(ty) = ty {
                return Ok(Some(ty));
            }
            **expression = Expr::Cast {
                implicit: true,
                expr: expression.clone(),
                ty: "text".into(),
            };
            Ok(Some(ColumnType::Text))
        })
        .collect()
}

fn finish_index_keys(
    context: &SchemaBindingContext<'_, '_>,
    statement: &mut CreateIndex,
    expressions: &[Option<ColumnType>],
) -> Result<Vec<ColumnType>, SQLError> {
    let definitions = context
        .catalog
        .schema_expression_columns(&statement.table)?
        .ok_or_else(|| SQLError::UnknownTable(statement.table.clone()))?;
    let mut types = Vec::with_capacity(statement.columns.len());
    let btree = matches!(
        super::options::index_access_method(statement)?.as_str(),
        "" | "btree"
    );
    statement.column_order.resize(
        statement.columns.len(),
        crate::ast::IndexColumnOrder::default(),
    );
    for (position, (key, analyzed)) in statement.columns.iter_mut().zip(expressions).enumerate() {
        let order = &mut statement.column_order[position];
        let written = statement
            .key_collations
            .get(position)
            .and_then(Option::as_deref);
        match key {
            IndexKey::Column(name) => {
                let Some(column) = definitions.iter().find(|column| column.name == *name) else {
                    if definitions.is_empty() {
                        types.push(ColumnType::Text);
                        continue;
                    }
                    return Err(SQLError::UnknownColumn(name.clone()));
                };
                let natural = column.collation_oid.unwrap_or_else(|| {
                    u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
                        &column.ty,
                    ))
                    .expect("type collation OID")
                });
                let selected = super::index_key_collation(
                    context.catalog,
                    Some(&column.ty),
                    natural,
                    written,
                    order.collation_oid,
                )?;
                order.overrides_collation = selected != natural;
                order.collation_oid = order.overrides_collation.then_some(selected);
                if btree {
                    require_default_btree_operator_class(&column.ty)?;
                }
                types.push(column.ty.clone());
            }
            IndexKey::Expression(expression) => {
                let clause = crate::catalog::collations::expression::ast_clause(expression)?
                    .map(|clause| {
                        crate::catalog::collations::expression::select(
                            clause.name,
                            clause.oid,
                            analyzed.as_ref(),
                            Some(context.catalog),
                        )
                    })
                    .transpose()?;
                while crate::catalog::collations::expression::discard_ast_clause(expression)? {}
                let natural = super::plan_index_expression(
                    context.catalog,
                    &statement.table,
                    expression,
                    false,
                )?;
                let ty = analyzed
                    .as_ref()
                    .expect("analyzed index expression")
                    .clone();
                let selected = super::index_key_collation(
                    context.catalog,
                    Some(&ty),
                    natural,
                    written,
                    clause.or(order.collation_oid),
                )?;
                order.overrides_collation = natural != selected;
                order.collation_oid = Some(selected);
                if btree {
                    require_default_btree_operator_class(&ty)?;
                }
                let column = match expression.as_ref() {
                    crate::ast::Expr::Column(name) => Some(name.clone()),
                    crate::ast::Expr::Cast { expr, .. } => {
                        if let crate::ast::Expr::Column(name) = expr.as_ref() {
                            definitions
                                .iter()
                                .any(|column| column.name == *name && column.ty == ty)
                                .then(|| name.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(column) = column {
                    *key = IndexKey::Column(column.to_string());
                }
                types.push(ty);
            }
        }
    }
    // As in `PostgreSQL`, an included column may repeat and may also be a key column.
    for name in &statement.included_columns {
        match definitions.iter().find(|column| column.name == *name) {
            Some(_) => {}
            None if definitions.is_empty() => {}
            None => return Err(SQLError::UnknownColumn(name.clone())),
        }
    }
    reject_unsupported_included_columns(statement)?;
    for key in &statement.columns {
        match key {
            IndexKey::Column(name) => {
                if let Some(column) = definitions.iter().find(|column| column.name == *name) {
                    reject_virtual_generated_column(column)?;
                }
            }
            IndexKey::Expression(expression) => {
                for column in &definitions {
                    if column
                        .generated
                        .as_ref()
                        .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
                        && crate::schema::dependencies::schema_expr_references_column(
                            expression,
                            &column.name,
                        )
                    {
                        return Err(SQLError::Unsupported(format!(
                            "index expressions cannot use virtual generated column `{}`",
                            column.name
                        )));
                    }
                }
            }
        }
    }
    for name in &statement.included_columns {
        if let Some(column) = definitions.iter().find(|column| column.name == *name) {
            reject_virtual_generated_column(column)?;
        }
    }
    Ok(types)
}

fn reject_virtual_generated_column(column: &crate::ast::ColumnDef) -> Result<(), SQLError> {
    if column
        .generated
        .as_ref()
        .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
    {
        return Err(SQLError::Unsupported(
            "indexes on virtual generated columns are not supported".into(),
        ));
    }
    Ok(())
}

/// Only a btree index carries columns beside its key.
fn reject_unsupported_included_columns(statement: &CreateIndex) -> Result<(), SQLError> {
    let method = statement.access_method.to_ascii_lowercase();
    if statement.included_columns.is_empty() || matches!(method.as_str(), "" | "btree") {
        return Ok(());
    }
    Err(SQLError::Unsupported(format!(
        "access method \"{method}\" does not support included columns"
    )))
}

/// Restored declarations without retained attribute names use their original key syntax.
pub fn attribute_names(statement: &CreateIndex) -> std::borrow::Cow<'_, [String]> {
    if !statement.attribute_names.is_empty() {
        return std::borrow::Cow::Borrowed(&statement.attribute_names);
    }
    let keys: Vec<_> = statement
        .columns
        .iter()
        .cloned()
        .chain(
            statement
                .included_columns
                .iter()
                .cloned()
                .map(IndexKey::Column),
        )
        .collect();
    std::borrow::Cow::Owned(key_names(&keys))
}

/// Bind keys and predicates and retain the public attribute names assigned before expression simplification.
pub fn prepare_index_definition(
    catalog: &dyn crate::schema::SchemaExpressionCatalog,
    bindings: &dyn crate::semantics::conflict::InferenceBindingScope,
    c: &mut CreateIndex,
) -> Result<crate::catalog::index::IndexDefinition, SQLError> {
    let key_names = attribute_names(c).into_owned();
    let binding = bindings.binding_scope()?;
    let scope = binding.context();
    let context = SchemaBindingContext {
        catalog,
        binding: &scope,
    };
    let expressions = bind_index_keys(&context, c)?;
    if let Some(predicate) = c.predicate.as_deref_mut() {
        super::bind_index_predicate(catalog, &scope, &c.table, predicate)?;
    }
    super::options::index_access_method(c)?;
    if c.unique {
        super::unique::validate_unique_index_method(c)?;
    }
    reject_unsupported_included_columns(c)?;
    if let Some(predicate) = c.predicate.as_deref_mut() {
        super::plan_index_expression(catalog, &c.table, predicate, true)?;
    }
    super::options::validate_index_options(c)?;
    let key_types = finish_index_keys(&context, c, &expressions)?;
    Ok(crate::catalog::index::IndexDefinition {
        catalog: None,
        relationships: crate::catalog::index::IndexRelationships::default(),
        key_names,
        key_types,
        included_columns: c.included_columns.clone(),
        column_order: c.column_order.clone(),
        predicate: c.predicate.clone(),
        unique: c.unique,
        nulls_not_distinct: c.nulls_not_distinct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_selection_names_use_the_selected_attribute_through_casts() {
        let crate::ast::Statement::CreateIndex(statement) =
            crate::compile("CREATE INDEX ix ON t (((ROW(id, 2)).f1), (((ROW(id, 2)).f1)::bigint))")
                .unwrap()
                .remove(0)
        else {
            panic!("index")
        };
        assert_eq!(key_names(&statement.columns), ["f1", "f11"]);
    }
}
