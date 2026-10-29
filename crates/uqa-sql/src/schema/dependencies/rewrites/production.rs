//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read sequence dependencies through the shared schema traversal with admitted syntax scratch.

use crate::{
    ast::{ColumnType, Expr},
    SQLError,
};
use uqa_core::{
    memory::{MemoryReservation, ProductionControl},
    Value,
};

pub fn visit_sequence_function_references_with_control(
    expression: &Expr,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    // Keep syntax storage ahead of its lease in drop order, including early traversal failures.
    struct Scratch {
        expression: Expr,
        _memory: Option<MemoryReservation>,
    }
    let (expression, memory) =
        crate::plan::copy::copy_syntax_with_control(expression, control)?.into_parts();
    let mut scratch = Scratch {
        expression,
        _memory: memory,
    };
    let mut failure = None;
    let status = super::super::walk_schema_expr_mut(&mut scratch.expression, &mut |node| {
        let result = (|| {
            control.check()?;
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
            Ok::<_, SQLError>(())
        })();
        if let Err(error) = result {
            failure = Some(error);
            return Err(String::new());
        }
        Ok(())
    });
    if let Some(error) = failure {
        return Err(error);
    }
    status.map_err(SQLError::Internal)
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
