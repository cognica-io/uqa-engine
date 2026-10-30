//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Analysis, RelationColumns, SQLError};

#[expect(
    clippy::too_many_arguments,
    reason = "borrows function and function-group descriptors without materializing a source"
)]
pub(super) fn columns(
    context: Analysis<'_, '_>,
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    width: usize,
    output_name: &str,
    alias: Option<&str>,
    aliases: &[String],
    typed: bool,
    ordinality: bool,
) -> Result<RelationColumns, SQLError> {
    let control = &context.control;
    control.check()?;
    if let Some((binding, function)) = binding.and_then(|binding| {
        crate::routines::selected_bound_function(context.routines, binding)
            .map(|function| (binding, function))
    }) {
        let names = crate::semantics::user_table_function_columns_with_control(
            context.routines,
            &function,
            binding.invocation.as_deref(),
            output_name,
            alias,
            aliases,
            ordinality,
            control,
        )?;
        return RelationColumns::known(names, control);
    }
    let builtin = binding.is_none_or(|binding| binding.builtin)
        && crate::semantics::is_builtin_table_function(
            &crate::semantics::builtin_function_dispatch_name_with_control(name, control)?,
        );
    let names = if builtin {
        crate::semantics::table_function_empty_schema_with_control(
            name,
            output_name,
            alias,
            aliases,
            width,
            ordinality,
            control,
        )?
    } else {
        crate::semantics::apply_table_function_aliases_with_control(
            aliases,
            &[],
            ordinality,
            control,
        )?
    };
    let mut columns = RelationColumns::known(names, control)?;
    columns.complete = builtin || typed;
    Ok(columns)
}
