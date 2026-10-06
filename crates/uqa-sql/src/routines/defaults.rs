//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parameter defaults are analyzed in declaration order, independently of body validation.

use super::compilation::RoutineCompilationContext;
use crate::{
    ast::{Expr, RoutineDefaultType},
    binding::{bind_expression_plan_routines_for_storage, syntax_sites::expression_syntax_sites},
    catalog::stored_ast,
    plan::ExpressionPlan,
    type_resolution::{
        assignment_type_compatible, match_routine_signature, routine_polymorphic_type,
        RoutineCallDescriptor, RoutineParameterDescriptor, RoutinePolymorphicType,
    },
    ColumnType, RowSchema, SQLError,
};
use uqa_core::Value;

/// Transform a parameter default and check its assignment to the declared type, as
/// `interpret_function_parameter_list` does before visiting the next parameter.
/// Only unknown constants are read here; calls and domain checks remain delayed.
pub(super) fn analyze_parameter_default(
    context: &RoutineCompilationContext<'_>,
    expression: &mut Expr,
    type_name: &str,
) -> Result<Option<RoutineDefaultType>, SQLError> {
    let aggregate = |name: &str| context.catalog.has_registered_aggregate_function(name);
    let original = ExpressionPlan::lower_with(expression.clone(), &aggregate);
    let mut bound = original.clone();
    let binding = context.catalog.binding_snapshot()?;
    let source = crate::binding::analyze_default_inputs(
        context.routines,
        &aggregate,
        context.catalog,
        &mut bound,
        &binding.context(),
    )?;
    let polymorphic = routine_polymorphic_type(type_name);
    let target = if let Some(polymorphic) = polymorphic {
        check_polymorphic_default(
            context,
            type_name,
            polymorphic,
            source.as_ref(),
            matches!(expression, Expr::Literal(Value::Null)),
        )?;
        None
    } else {
        let target = context
            .types
            .resolve_catalog_column_type_name(type_name)?
            .without_type_modifiers();
        if let Some(source) = &source {
            if !assignment_type_compatible(source, &target) {
                return Err(type_mismatch(
                    context,
                    &context.types.format_type(&target)?,
                    source,
                )?);
            }
        }
        Some(target)
    };
    bind_expression_plan_routines_for_storage(
        context.routines,
        &mut bound,
        &[],
        &binding.context(),
        &RowSchema::default(),
    )?;
    let sites = expression_syntax_sites(&original, &bound)?;
    stored_ast::bind_stored_expression_sites(expression, &sites)?;
    if let Some(target) = &target {
        if matches!(expression, Expr::Literal(Value::Str(_) | Value::Null)) {
            stored_ast::read_unknown_stored_literal(
                context.routines.enum_labels(),
                context.routines.catalog_input_functions(),
                expression,
                target,
                false,
            )?;
        }
    }
    crate::schema::dependencies::oid_alias::read_oid_alias_constants(context.catalog, expression)?;
    // Input errors belong to parameter analysis; dependency rejection follows
    // declaration and SQL-standard body validation in routine compilation.
    let mut regroles = crate::catalog::regrole_dependencies::StoredRegroleConstants::default();
    regroles.collect_expression(expression, target.as_ref());
    regroles.validate_inputs_with(context.regroles)?;
    Ok(default_expression_type(
        type_name,
        expression,
        target.or(source),
    ))
}

/// Reconstruct the same annotation when restoring an older routine definition.
pub(super) fn default_expression_type(
    type_name: &str,
    expression: &Expr,
    source: Option<ColumnType>,
) -> Option<RoutineDefaultType> {
    if let Some(source) = source {
        return Some(RoutineDefaultType::Concrete(source));
    }
    let polymorphic = routine_polymorphic_type(type_name)?;
    if matches!(expression, Expr::Literal(Value::Null))
        && !matches!(
            polymorphic,
            RoutinePolymorphicType::AnyElement
                | RoutinePolymorphicType::AnyNonArray
                | RoutinePolymorphicType::AnyCompatible
                | RoutinePolymorphicType::AnyCompatibleNonArray
        )
    {
        return Some(RoutineDefaultType::Polymorphic(type_name.into()));
    }
    None
}

fn check_polymorphic_default(
    context: &RoutineCompilationContext<'_>,
    name: &str,
    polymorphic: RoutinePolymorphicType,
    source: Option<&ColumnType>,
    is_null: bool,
) -> Result<(), SQLError> {
    let Some(source) = source else {
        if polymorphic == RoutinePolymorphicType::AnyEnum {
            return Err(error(
                "42804",
                "argument of DEFAULT must be type anyenum, not type unknown",
            ));
        }
        if is_null
            || matches!(
                polymorphic,
                RoutinePolymorphicType::AnyElement
                    | RoutinePolymorphicType::AnyNonArray
                    | RoutinePolymorphicType::AnyCompatible
                    | RoutinePolymorphicType::AnyCompatibleNonArray
            )
        {
            return Ok(());
        }
        return Err(error(
            "0A000",
            format!("cannot accept a value of type {name}"),
        ));
    };
    let parameters = [RoutineParameterDescriptor {
        name: None,
        type_name: name.to_string(),
        column_type: None,
        has_default: false,
        default_type: None,
        variadic: false,
    }];
    let matches = match_routine_signature(
        &parameters,
        RoutineCallDescriptor {
            argument_names: &[None],
            argument_types: &[Some(source.clone())],
            explicit_variadic: false,
        },
    );
    if matches.is_ok_and(|matched| matched.is_some()) {
        Ok(())
    } else {
        Err(type_mismatch(context, name, source)?)
    }
}

fn type_mismatch(
    context: &RoutineCompilationContext<'_>,
    target: &str,
    source: &ColumnType,
) -> Result<SQLError, SQLError> {
    Ok(error(
        "42804",
        format!(
            "argument of DEFAULT must be type {target}, not type {}",
            context.types.format_type(source)?
        ),
    ))
}

fn error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
