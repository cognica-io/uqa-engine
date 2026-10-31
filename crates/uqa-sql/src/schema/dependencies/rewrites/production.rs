//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow sequence dependencies through the same ordered traversal as stored-expression rewrites.

use crate::{
    ast::{ColumnType, Expr},
    SQLError,
};
use uqa_core::{memory::ProductionControl, Value};

#[cfg(test)]
mod tests;

pub fn visit_sequence_function_references_with_control(
    expression: &Expr,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    super::super::walk_schema_expr_with_control(expression, control, &mut |node| {
        if let Expr::Func {
            name,
            binding,
            args,
            ..
        } = node
        {
            if super::is_sequence_function(name, binding.as_ref()) {
                if let Some(reference) = args
                    .first()
                    .map(|arg| literal(arg, control))
                    .transpose()?
                    .flatten()
                {
                    visit(reference)?;
                }
            }
        }
        Ok(())
    })
}

fn literal<'a>(
    expression: &'a Expr,
    control: &ProductionControl<'_>,
) -> Result<Option<&'a str>, SQLError> {
    match expression {
        Expr::Literal(Value::Str(reference)) => Ok(Some(reference)),
        Expr::Cast { expr, ty, .. } => match ColumnType::from_sql_name_with_control(ty, control) {
            Ok(ty) if matches!(*ty, ColumnType::Regclass) => literal(expr, control),
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}
