//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected range function borrows its input until a complete derived source is ready to replace it.

use crate::{optimizer::owned::Context, scratch::Buffer};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{
    ast::{ColumnType, FunctionBinding, FunctionOrderSyntax},
    plan::{
        MutableQueryPlan, MutableSourcePlan, OwnedProjection, OwnedScalarExpr, RelationalPlan,
        SourcePlan,
    },
    SQLError, ScalarExpr,
};

struct RangeCall<'a> {
    name: &'a str,
    binding: &'a FunctionBinding,
    output_name: &'a str,
    args: &'a [ScalarExpr],
    alias: Option<&'a str>,
    columns: &'a [String],
    types: &'a [String],
    ordinality: bool,
    group_columns: &'a [String],
}

impl<'a> RangeCall<'a> {
    fn selected(source: &'a SourcePlan) -> Option<Self> {
        match source {
            SourcePlan::Function {
                name,
                binding: Some(binding),
                output_name,
                args,
                alias,
                column_aliases,
                column_types,
                ordinality,
                relations: None,
            } => Some(Self {
                name,
                binding,
                output_name,
                args,
                alias: alias.as_deref(),
                columns: column_aliases,
                types: column_types,
                ordinality: *ordinality,
                group_columns: &[],
            }),
            SourcePlan::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality: false,
            } => {
                let [member] = functions.as_slice() else {
                    return None;
                };
                if member.relations.is_some() {
                    return None;
                }
                Some(Self {
                    name: &member.name,
                    binding: member.binding.as_ref()?,
                    output_name: &member.output_name,
                    args: &member.args,
                    alias: alias.as_deref(),
                    columns: &member.column_aliases,
                    types: &member.column_types,
                    ordinality: false,
                    group_columns: column_aliases,
                })
            }
            _ => None,
        }
    }

    fn derived(
        &self,
        query: MutableQueryPlan,
        columns: Produced<Vec<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<MutableSourcePlan, SQLError> {
        let columns = alias_columns(columns, self.group_columns, control)?;
        MutableSourcePlan::subquery(
            query,
            Some(control.copy_text(self.alias.unwrap_or(self.output_name))?),
            columns,
            None,
            true,
            control,
        )
    }
}

fn alias_columns(
    columns: Produced<Vec<String>>,
    aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    if aliases.is_empty() {
        Ok(columns)
    } else {
        uqa_sql::semantics::apply_table_function_aliases_with_control(
            &columns, aliases, false, control,
        )
    }
}

pub(super) fn expand(
    source: &SourcePlan,
    context: &Context<'_, '_>,
) -> Result<Option<MutableSourcePlan>, SQLError> {
    let control = &context.config.production;
    control.check()?;
    let Some(routines) = context.config.routine_inlining.as_ref() else {
        return Ok(None);
    };
    let Some(call) = RangeCall::selected(source) else {
        return Ok(None);
    };
    if !call.ordinality {
        if let Some(inlined) = routines.prepare_table_owned(
            call.binding,
            call.args,
            context.config.active_inline_routines,
            call.columns,
            call.types,
            control,
        )? {
            let query = optimize_inlined(inlined.query, inlined.identity, context)?;
            let declared = match inlined.declared_columns {
                Some(columns) => columns,
                None => {
                    let mut columns = ProductionVec::new(*control);
                    columns.push_produced(
                        control.copy_text(call.alias.unwrap_or(call.output_name))?,
                    )?;
                    columns.finish()?
                }
            };
            let columns = alias_columns(declared, call.columns, control)?;
            return call.derived(query, columns, control).map(Some);
        }
    }
    if !call.types.is_empty() {
        return Ok(None);
    }
    let Some(columns) = routines.scalar_from_columns_with_control(
        call.binding,
        call.args,
        call.output_name,
        call.alias,
        call.columns,
        call.ordinality,
        control,
    )?
    else {
        return Ok(None);
    };
    let mut arguments = Buffer::new(*control);
    for argument in call.args {
        arguments.push(argument.copy_for_rewrite(control)?)?;
    }
    let expression = OwnedScalarExpr::call(
        control.copy_text(call.name)?,
        Some(call.binding.clone_with_control(control)?),
        arguments.into_iter(),
        control,
    )?;
    let expression = super::scalar::owned::optimize(expression, context.config)?;
    if same_call(&expression, call.name, call.binding, call.args, control)? {
        return Ok(None);
    }
    let query = scalar_query(expression, &columns, call.ordinality, control)?;
    call.derived(query, columns, control).map(Some)
}

fn optimize_inlined(
    query: MutableQueryPlan,
    identity: [u8; 16],
    context: &Context<'_, '_>,
) -> Result<MutableQueryPlan, SQLError> {
    let control = &context.config.production;
    let query = unit_source(query, control)?;
    context.config.with_inline_routine(identity, |nested| {
        Context::admitted(nested, context.aggregates).query(query)
    })
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
