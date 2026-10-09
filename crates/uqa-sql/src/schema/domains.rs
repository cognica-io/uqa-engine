//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain declaration validation, inherited defaults, and constraint binding.

use super::SchemaBindingContext;
use crate::assignment::domain::domain_error;
use crate::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use crate::{
    ast::{ColumnType, CreateDomain, DomainCheck, DomainNotNull, Expr},
    catalog::domain::DomainCatalog,
    RowSchema, SQLError,
};
use std::collections::BTreeSet;
use uqa_core::Value;

pub fn prepare_domain_definition(
    context: &SchemaBindingContext<'_, '_>,
    domains: &dyn DomainCatalog,
    definition: &mut CreateDomain,
    schema_names: &BTreeSet<String>,
) -> Result<(), SQLError> {
    definition.base =
        crate::type_resolution::resolve_declared_column_type(context.catalog, &definition.base)?;
    if definition.default.is_none() {
        definition.default =
            crate::catalog::domain::domain_default_expression(domains, &definition.base);
    }
    if matches!(
        definition.base,
        ColumnType::Void | ColumnType::Record | ColumnType::AnyArray
    ) {
        return Err(domain_error(
            "42804",
            format!(
                "\"{}\" is not a valid base type for a domain",
                definition.base.sql_name()
            ),
        ));
    }
    // DefineDomain requires USAGE on the base type once it is known to be valid for a domain.
    context.catalog.require_type_usage(&definition.base)?;
    if definition.collation.is_some() {
        return Err(SQLError::Unsupported(
            "domain collation binding is not implemented".into(),
        ));
    }
    if let Some(default) = &mut definition.default {
        // `DefineDomain` cooks the default against the base type and names the domain as the column.
        let domain = definition
            .name
            .rsplit('.')
            .next()
            .unwrap_or(definition.name.as_str());
        if !super::defaults::validate_default_expression(
            context,
            default,
            &definition.base,
            domain,
        )? {
            definition.default = None;
        }
    }
    constraints::assign_names(definition, schema_names)?;
    for check in &mut definition.checks {
        bind_domain_check(context, &definition.base, &mut check.expression)?;
    }
    definition
        .checks
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok(())
}

/// Name and bind only a newly added CHECK. Existing defaults and constraints already carry their catalog identities and must not be analyzed again in the ALTER statement's namespace.
pub fn prepare_added_check(
    context: &SchemaBindingContext<'_, '_>,
    definition: &CreateDomain,
    check: DomainCheck,
    schema_names: &BTreeSet<String>,
) -> Result<DomainCheck, SQLError> {
    let mut named = definition.clone();
    named.checks.push(check);
    constraints::assign_names(&mut named, schema_names)?;
    let mut check = named.checks.pop().expect("new domain CHECK");
    bind_domain_check(context, &definition.base, &mut check.expression)?;
    Ok(check)
}

/// Name a new NOT NULL constraint after the executor has resolved and locked its domain. An existing NOT NULL constraint is retained unchanged, as ALTER DOMAIN does even when the requested name differs.
pub fn prepare_added_not_null(
    definition: &CreateDomain,
    constraint: DomainNotNull,
    schema_names: &BTreeSet<String>,
) -> Result<DomainNotNull, SQLError> {
    if let Some(existing) = &definition.not_null {
        return Ok(existing.clone());
    }
    let mut named = definition.clone();
    named.not_null = Some(constraint);
    constraints::assign_names(&mut named, schema_names)?;
    Ok(named.not_null.expect("new domain NOT NULL"))
}

/// Restore predecessor range syntax and composite constructors while retaining stored catalog identities.
pub fn restore_composite_constructors(
    context: &SchemaBindingContext<'_, '_>,
    definition: &mut CreateDomain,
) -> Result<bool, SQLError> {
    let mut changed = false;
    if let Some(default) = &mut definition.default {
        changed |= default.upgrade_legacy_serialized_dispatches();
        if crate::type_resolution::composite_rows::expression_requires_binding(
            default,
            context.catalog,
        )? {
            changed |=
                super::defaults::bind_stored_schema_expression(context, default, default.clone())?;
        }
    }
    for check in &mut definition.checks {
        changed |= check.expression.upgrade_legacy_serialized_dispatches();
        if crate::type_resolution::composite_rows::expression_requires_binding(
            &check.expression,
            context.catalog,
        )? {
            bind_domain_check(context, &definition.base, &mut check.expression)?;
            changed = true;
        }
    }
    Ok(changed)
}

struct DomainValueResolver<'a>(&'a ColumnType);

impl VariableResolver for DomainValueResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if name != "value" {
            return Err(SQLError::UnknownColumn(name.into()));
        }
        Ok(Some(ResolvedVariable {
            value: Value::Null,
            declared_type: Some(self.0.catalog_name()),
        }))
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Err(SQLError::UnknownTable(qualifier.into()))
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Err(domain_error(
            "42P02",
            format!("there is no parameter ${index}"),
        ))
    }
}

fn bind_domain_check(
    context: &SchemaBindingContext<'_, '_>,
    base: &ColumnType,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let original = crate::plan::ExpressionPlan::lower(expression.clone());
    let mut plan = original.clone();
    let input = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
    let ty =
        crate::binding::analyze_domain_check(context.catalog, &mut plan, &input, context.binding)?;
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&original, &plan)?;
    crate::catalog::stored_ast::bind_stored_expression_sites(expression, &sites)?;
    if let Some(mut ty) = ty.as_ref() {
        while let ColumnType::Domain { base, .. } = ty {
            ty = base;
        }
        if *ty != ColumnType::Boolean {
            return Err(domain_error(
                "42804",
                format!(
                    "argument of CHECK must be type boolean, not type {}",
                    ty.regtype_name()
                ),
            ));
        }
    } else if matches!(expression, Expr::Literal(Value::Null | Value::Str(_))) {
        crate::catalog::stored_ast::read_unknown_stored_literal(
            crate::FunctionTypeResolver::enum_labels(context.catalog),
            crate::FunctionTypeResolver::catalog_input_functions(context.catalog),
            expression,
            &ColumnType::Boolean,
            false,
        )?;
    } else {
        *expression = Expr::Cast {
            implicit: true,
            expr: Box::new(expression.clone()),
            ty: "boolean".into(),
        };
    }
    // The stored syntax includes the boolean cast, so its typed copy does too.
    let typed = bind_expr(expression, &mut DomainValueResolver(base))?;
    super::defaults::bind_stored_schema_expression(context, expression, typed)?;
    Ok(())
}

pub mod constraints;
pub mod dependencies;
pub mod removal;

#[cfg(test)]
mod tests;
