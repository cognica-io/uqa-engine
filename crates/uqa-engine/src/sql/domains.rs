//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain declaration binding and conversion-time constraint evaluation.

use std::collections::BTreeSet;

use uqa_core::Value;
use uqa_execution::RowSchema;
use uqa_sql::ast::{ColumnType, CreateDomain, Expr};
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::{ResultRow, SQLError};

use crate::engine_domains::StoredDomain;
use crate::{Engine, RelationIdentity};

pub(crate) fn resolve_declared_column_type(
    engine: &Engine,
    ty: &ColumnType,
) -> Result<ColumnType, SQLError> {
    match ty {
        ColumnType::Named(name) => super::resolve_catalog_column_type_name(engine, name),
        ColumnType::Array(element) => resolve_declared_column_type(engine, element)
            .map(|element| ColumnType::Array(Box::new(element))),
        other => Ok(other.clone()),
    }
}

pub(super) fn create_domain(engine: &Engine, mut definition: CreateDomain) -> Result<(), SQLError> {
    engine.prepare_explicit_transaction_writer()?;
    definition.name = engine.try_relation_name_for_sql_create(&definition.name)?;
    let identity =
        RelationIdentity::from_legacy_name(&definition.name).map_err(SQLError::Internal)?;
    if super::resolve_catalog_column_type(engine, &definition.name).is_some()
        || engine
            .try_table(&definition.name)
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .is_some()
    {
        return Err(domain_error(
            "42710",
            format!("type \"{}\" already exists", identity.name),
        ));
    }
    definition.base = resolve_declared_column_type(engine, &definition.base)?;
    if definition.default.is_none() {
        definition.default = engine.domain_default_expression(&definition.base);
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
        super::ddl::validate_default_expression(engine, default, &definition.base)?;
        if let Expr::Literal(Value::Str(value)) = default {
            let mut base = &definition.base;
            while let ColumnType::Domain { base: parent, .. } = base {
                base = parent;
            }
            uqa_sql::expr::cast_value_with_type_resolution(
                &Value::Str(value.clone()),
                None,
                &base.without_type_modifiers().sql_name(),
                Some(engine),
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
        bind_domain_check(engine, &definition.base, &mut check.expression)?;
    }
    definition
        .checks
        .sort_by(|left, right| left.name.cmp(&right.name));
    let object_id = crate::new_nonzero_catalog_identity("domain", "object identity")
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let oid = super::catalog::domain_object_oid(&object_id);
    engine.publish_domain(StoredDomain {
        object_id,
        oid,
        identity,
        owner: engine.current_user_name(),
        definition,
    })
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
    engine: &Engine,
    base: &ColumnType,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let typed = bind_expr(expression, &mut DomainValueResolver(base))?;
    let plan = uqa_planner::ExpressionPlan::lower(typed.clone());
    if !plan.subqueries.is_empty() {
        return Err(domain_error(
            "0A000",
            "cannot use subquery in check constraint",
        ));
    }
    if super::window::expr_has_window(&plan.scalar) {
        return Err(domain_error(
            "42P20",
            "window functions are not allowed in check constraints",
        ));
    }
    if super::aggregates::contains_aggregate(engine, &plan.scalar) {
        return Err(domain_error(
            "42803",
            "aggregate functions are not allowed in check constraints",
        ));
    }
    let ty = uqa_execution::common_context_expression_type(
        &plan.scalar,
        &RowSchema::default(),
        &[],
        Some(engine),
    )?;
    if let Some(ty) = ty {
        if uqa_sql::expr::coercion_type_name(&ty) != "boolean" {
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
    super::ddl::bind_stored_schema_expression_routines(engine, expression, typed)?;
    Ok(())
}

pub(crate) fn cast_domain_value(
    engine: &Engine,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
) -> Result<Option<Value>, SQLError> {
    convert_domain_value(engine, value, source, ty, false)
}

pub(crate) fn assign_domain_value(
    engine: &Engine,
    value: &Value,
    ty: &ColumnType,
) -> Result<Option<Value>, SQLError> {
    convert_domain_value(engine, value, None, ty, true)
}

fn convert_domain_value(
    engine: &Engine,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
    assignment: bool,
) -> Result<Option<Value>, SQLError> {
    let ColumnType::Domain { oid, .. } = ty else {
        return Ok(None);
    };
    let Some(domain) = engine.domain_by_oid(*oid) else {
        return Ok(None);
    };
    if source
        .and_then(|name| super::resolve_catalog_column_type(engine, name))
        .as_ref()
        == Some(ty)
    {
        return Ok(Some(value.clone()));
    }
    let mut chain = vec![domain.clone()];
    let mut base = domain.definition.base.clone();
    while let ColumnType::Domain {
        oid,
        base: underlying,
        ..
    } = &base
    {
        if let Some(parent) = engine.domain_by_oid(*oid) {
            base = parent.definition.base.clone();
            chain.push(parent);
        } else {
            base = *underlying.clone();
        }
    }
    let value = if assignment {
        super::ddl::convert_value_to_column_type_with_engine(engine, value.clone(), &base)?
    } else {
        uqa_sql::expr::cast_value_with_type_resolution(
            value,
            source,
            &base.sql_name(),
            Some(engine),
        )?
    };
    if matches!(value, Value::Null)
        && chain
            .iter()
            .any(|domain| domain.definition.not_null.is_some())
    {
        return Err(domain_error(
            "23502",
            format!("domain {} does not allow null values", domain.identity.name),
        ));
    }
    let row = ResultRow::from([("value".into(), value.clone())]);
    let schema = RowSchema::with_types(
        vec!["value".into()],
        vec![Some(domain.definition.base.clone())],
    );
    for check in chain
        .iter()
        .rev()
        .flat_map(|domain| &domain.definition.checks)
    {
        let result = super::scalar::eval_lowered_expression_with_schema(
            engine,
            &check.expression,
            &row,
            &schema,
            &[],
        )?;
        if result == Value::Bool(false) {
            return Err(domain_error(
                "23514",
                format!(
                    "value for domain {} violates check constraint \"{}\"",
                    domain.identity.name,
                    check.name.as_deref().expect("bound domain constraint")
                ),
            ));
        }
    }
    Ok(Some(value))
}

fn domain_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
