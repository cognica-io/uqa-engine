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
    ast::{ColumnType, CreateDomain, Expr},
    catalog::domain::DomainCatalog,
    RowSchema, SQLError,
};
use std::collections::BTreeSet;
use uqa_core::{RelationIdentity, Value};

pub fn prepare_domain_definition(
    context: &SchemaBindingContext<'_, '_>,
    domains: &dyn DomainCatalog,
    definition: &mut CreateDomain,
) -> Result<(), SQLError> {
    let identity =
        RelationIdentity::from_legacy_name(&definition.name).map_err(SQLError::Internal)?;
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
            "42809",
            format!(
                "\"{}\" is not a valid base type for a domain",
                definition.base.sql_name()
            ),
        ));
    }
    if definition.collation.is_some() {
        return Err(SQLError::Unsupported(
            "domain collation binding is not implemented".into(),
        ));
    }
    if let Some(default) = &mut definition.default {
        super::defaults::validate_default_expression(context, default, &definition.base)?;
        if let Expr::Literal(Value::Str(value)) = default {
            let mut base = &definition.base;
            while let ColumnType::Domain { base: parent, .. } = base {
                base = parent;
            }
            crate::expr::cast_value_with_type_resolution(
                &Value::Str(value.clone()),
                None,
                &base.without_type_modifiers().sql_name(),
                Some(context.catalog),
            )?;
        }
    }
    let mut names = BTreeSet::new();
    if let Some(not_null) = &mut definition.not_null {
        let name = not_null
            .name
            .get_or_insert_with(|| format!("{}_not_null", identity.name));
        names.insert(name.clone());
    }
    for check in &mut definition.checks {
        if let Some(name) = &check.name {
            if !names.insert(name.clone()) {
                return Err(domain_error(
                    "42710",
                    format!(
                        "constraint \"{name}\" for domain \"{}\" already exists",
                        identity.name
                    ),
                ));
            }
        } else {
            let base = format!("{}_check", identity.name);
            let mut name = base.clone();
            let mut suffix = 1;
            while !names.insert(name.clone()) {
                name = format!("{base}{suffix}");
                suffix += 1;
            }
            check.name = Some(name);
        }
        bind_domain_check(context, &definition.base, &mut check.expression)?;
    }
    definition
        .checks
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok(())
}

struct DomainValueResolver<'a>(&'a ColumnType);

impl VariableResolver for DomainValueResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if name != "value" {
            return Err(SQLError::UnknownColumn(name.into()));
        }
        Ok(Some(ResolvedVariable {
            value: Value::Null,
            declared_type: Some(self.0.sql_name()),
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
    let typed = bind_expr(expression, &mut DomainValueResolver(base))?;
    let plan = crate::plan::ExpressionPlan::lower(typed.clone());
    if !plan.subqueries.is_empty() {
        return Err(domain_error(
            "0A000",
            "cannot use subquery in check constraint",
        ));
    }
    if crate::semantics::windows::expr_has_window(&plan.scalar) {
        return Err(domain_error(
            "42P20",
            "window functions are not allowed in check constraints",
        ));
    }
    if crate::semantics::aggregates::contains_aggregate(context.catalog, &plan.scalar) {
        return Err(domain_error(
            "42803",
            "aggregate functions are not allowed in check constraints",
        ));
    }
    let ty = crate::type_resolution::common_context_expression_type(
        &plan.scalar,
        &RowSchema::default(),
        &[],
        Some(context.catalog),
    )?;
    if let Some(ty) = ty {
        if crate::expr::coercion_type_name(&ty) != "boolean" {
            return Err(domain_error(
                "42804",
                format!(
                    "argument of CHECK must be type boolean, not type {}",
                    ty.sql_name()
                ),
            ));
        }
    } else {
        *expression = Expr::Cast {
            expr: Box::new(expression.clone()),
            ty: "boolean".into(),
        };
    }
    super::defaults::bind_stored_schema_expression_routines(context, expression, typed)?;
    Ok(())
}
