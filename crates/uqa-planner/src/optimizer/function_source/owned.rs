//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function range expansion retains the original source until a complete admitted replacement is selected.

use crate::{optimizer::owned::Context, scratch::Buffer};
use uqa_core::{
    memory::{ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{
    ast::{ColumnType, FunctionOrderSyntax},
    plan::{
        MutableQueryPlan, MutableSourcePlan, OwnedProjection, OwnedScalarExpr, RelationalPlan,
        SourcePlan,
    },
    SQLError, ScalarExpr,
};

pub(in crate::optimizer) fn optimize(
    source: MutableSourcePlan,
    context: &Context<'_, '_>,
) -> Result<MutableSourcePlan, SQLError> {
    let Some(routines) = context.config.routine_inlining.as_ref() else {
        return Ok(source);
    };
    let SourcePlan::Function {
        binding: Some(binding),
        name,
        output_name,
        args,
        alias,
        column_aliases,
        column_types,
        ordinality,
        relations: None,
    } = &*source
    else {
        return Ok(source);
    };
    let control = &context.config.production;
    if !*ordinality {
        if let Some(inlined) = routines.prepare_table_owned(
            binding,
            args,
            context.active,
            column_aliases,
            column_types,
            control,
        )? {
            let query = optimize_inlined(inlined.query, inlined.identity, context)?;
            let declared = match inlined.declared_columns {
                Some(columns) => columns,
                None => {
                    let mut columns = ProductionVec::new(*control);
                    columns.push_produced(
                        control.copy_text(alias.as_deref().unwrap_or(output_name))?,
                    )?;
                    columns.finish()?
                }
            };
            let columns = uqa_sql::semantics::apply_table_function_aliases_with_control(
                &declared,
                column_aliases,
                false,
                control,
            )?;
            let replacement = MutableSourcePlan::subquery(
                query,
                Some(control.copy_text(alias.as_deref().unwrap_or(output_name))?),
                columns,
                None,
                true,
                control,
            )?;
            drop(source);
            return Ok(replacement);
        }
    }
    if !column_types.is_empty() {
        return Ok(source);
    }
    let Some(columns) = routines.scalar_from_columns_with_control(
        binding,
        args,
        output_name,
        alias.as_deref(),
        column_aliases,
        *ordinality,
        control,
    )?
    else {
        return Ok(source);
    };
    let mut arguments = Buffer::new(*control);
    for argument in args {
        arguments.push(argument.copy_for_rewrite(control)?)?;
    }
    let expression = OwnedScalarExpr::call(
        control.copy_text(name)?,
        Some(binding.clone_with_control(control)?),
        arguments.into_iter(),
        control,
    )?;
    let expression = context.scalar(expression)?;
    if same_call(&expression, name, binding, args, control)? {
        return Ok(source);
    }
    let query = scalar_query(expression, &columns, *ordinality, control)?;
    let replacement = MutableSourcePlan::subquery(
        query,
        Some(control.copy_text(alias.as_deref().unwrap_or(output_name))?),
        columns,
        None,
        true,
        control,
    )?;
    drop(source);
    Ok(replacement)
}

fn optimize_inlined(
    query: MutableQueryPlan,
    identity: [u8; 16],
    context: &Context<'_, '_>,
) -> Result<MutableQueryPlan, SQLError> {
    let control = &context.config.production;
    let query = unit_source(query, control)?;
    let mut active = ProductionVec::new(*control);
    active.reserve(context.active.len() + 1)?;
    for identity in context.active {
        active.push_copy(*identity)?;
    }
    active.push_copy(identity)?;
    let active = active.finish()?;
    let nested = Context {
        config: context.config,
        aggregates: context.aggregates,
        active: &active,
    };
    nested.query(query)
}

pub(in crate::optimizer) fn optimize_group(
    source: MutableSourcePlan,
    context: &Context<'_, '_>,
) -> Result<MutableSourcePlan, SQLError> {
    let SourcePlan::FunctionGroup {
        functions,
        column_aliases,
        ordinality: false,
        ..
    } = &*source
    else {
        return Ok(source);
    };
    if functions.len() != 1 {
        return Ok(source);
    }
    let control = &context.config.production;
    let member = source.copy_function_group_member_for_rewrite(0, control)?;
    let member = optimize(member, context)?;
    let SourcePlan::Subquery {
        captured_output,
        lateral,
        alias,
        column_aliases: columns,
        ..
    } = &*member
    else {
        return Ok(source);
    };
    let captured_output = *captured_output;
    let lateral = *lateral;
    let alias = alias
        .as_ref()
        .map(|alias| control.copy_text(alias))
        .transpose()?;
    let columns = uqa_sql::semantics::apply_table_function_aliases_with_control(
        columns,
        column_aliases,
        false,
        control,
    )?;
    let query = member.into_query(control)?;
    let replacement =
        MutableSourcePlan::subquery(query, alias, columns, captured_output, lateral, control)?;
    drop(source);
    Ok(replacement)
}

fn same_call(
    expression: &ScalarExpr,
    expected_name: &str,
    expected_binding: &uqa_sql::ast::FunctionBinding,
    expected_args: &[ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let ScalarExpr::Func {
        name,
        binding,
        args,
        distinct,
        order_by,
        order_syntax,
        filter,
    } = expression
    else {
        return Ok(false);
    };
    if name != expected_name
        || binding.as_ref() != Some(expected_binding)
        || args.len() != expected_args.len()
    {
        return Ok(false);
    }
    for (left, right) in args.iter().zip(expected_args) {
        if !left.has_same_syntax_with_control(right, control)? {
            return Ok(false);
        }
    }
    Ok(!distinct
        && order_by.is_empty()
        && *order_syntax == FunctionOrderSyntax::Ordinary
        && filter.is_none())
}

fn scalar_query(
    expression: OwnedScalarExpr,
    columns: &[String],
    ordinality: bool,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let mut row = Buffer::new(*control);
    row.push(expression)?;
    if ordinality {
        row.push(OwnedScalarExpr::typed_literal(
            control.finish(Value::Int(1), control.empty_reservation())?,
            control.copy_text("bigint")?,
            Some(control.finish(ColumnType::BigInteger, control.empty_reservation())?),
            None,
            None,
            control,
        )?)?;
    }
    let mut projections = Buffer::new(*control);
    for (expression, name) in row.into_iter().zip(columns) {
        projections.push(OwnedProjection {
            expression,
            alias: Some(control.copy_text(name)?),
        })?;
    }
    unit_source(
        MutableQueryPlan::projected(projections.into_iter(), control)?,
        control,
    )
}

fn unit_source(
    query: MutableQueryPlan,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    if matches!(&query.root, RelationalPlan::QueryBlock(block) if block.from.is_none()) {
        query.replace_source(Some(MutableSourcePlan::unit(control)?), control)
    } else {
        Ok(query)
    }
}
