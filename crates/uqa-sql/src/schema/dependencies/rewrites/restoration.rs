//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Load-only schema validation observes the owning migration predicates without copying or rewriting declarations.

use crate::{
    ast::{ColumnDef, Expr, FunctionBinding, TableConstraintSet},
    SQLError,
};
use uqa_core::{memory::ProductionControl, Value};

/// Observe admitted scalar declarations in their original column/CHECK order. Current syntax remains borrowed; legacy dispatches, array bounds and range predicates use the same selection as initial-open migration.
pub fn has_legacy_schema_function_dispatches_with_control(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let mut legacy = false;
    let mut inspect = |expression: &Expr| {
        super::super::walk_schema_expr_with_control(expression, control, &mut |node| {
            legacy |= needs_restoration(node);
            Ok(())
        })
    };
    for column in columns {
        control.check()?;
        for expression in [column.default.as_ref(), column.check.as_ref()]
            .into_iter()
            .flatten()
        {
            inspect(expression)?;
        }
        if let Some(generated) = &column.generated {
            inspect(&generated.expression)?;
        }
    }
    for check in &constraints.checks {
        control.check()?;
        inspect(&check.expr)?;
    }
    Ok(legacy)
}

fn needs_restoration(expression: &Expr) -> bool {
    if crate::type_resolution::between::needs_ast_restoration(expression) {
        return true;
    }
    if let Expr::Func {
        name,
        binding,
        args,
        ..
    } = expression
    {
        FunctionBinding::legacy_serialized_dispatch(name, binding.as_ref()).is_some()
            || FunctionBinding::legacy_array_slice_bounds(binding.as_ref(), args.len(), |index| {
                matches!(args[index], Expr::Literal(Value::Null))
            })
            .is_some()
    } else {
        false
    }
}

#[cfg(test)]
mod tests;
