//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Signature checks for polymorphic array and variadic calls before result inference.

use crate::{ColumnType, SQLError, ScalarExpr};

pub(super) fn validate_variadic_prefix(
    name: &str,
    arguments: &[ScalarExpr],
    types: &[Option<&ColumnType>],
) -> Result<(), SQLError> {
    if crate::expr::variadic_any::minimum_arguments(name)
        .is_some_and(|minimum| arguments.len() < minimum)
    {
        return Err(crate::type_resolution::function_resolution_error(
            "42883",
            name,
            &crate::scalar_call_arguments(arguments)?
                .iter()
                .map(|argument| argument.name.map(str::to_string))
                .collect::<Vec<_>>(),
            &types.iter().map(|ty| ty.cloned()).collect::<Vec<_>>(),
            "does not exist",
        ));
    }
    Ok(())
}

pub(super) fn validate_array_call(
    name: &str,
    arguments: &[ScalarExpr],
    types: &[Option<&ColumnType>],
) -> Result<(), SQLError> {
    let arity = match name {
        "array_cat" | "array_append" | "array_prepend" | "array_remove" => 2,
        "array_replace" => 3,
        _ => return Ok(()),
    };
    let has_argument_markers = arguments.iter().any(|argument| {
        crate::scalar_call_argument(argument)
            .is_ok_and(|argument| argument.name.is_some() || argument.explicit_variadic)
    });
    if arguments.len() != arity || has_argument_markers {
        let names = arguments
            .iter()
            .map(|argument| {
                crate::scalar_call_argument(argument)
                    .map(|argument| argument.name.map(str::to_string))
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Err(crate::type_resolution::function_resolution_error(
            "42883",
            name,
            &names,
            &types.iter().map(|ty| ty.cloned()).collect::<Vec<_>>(),
            "does not exist",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn array_signatures_are_checked_before_result_type_inference() {
        for (sql, state) in [
            ("SELECT cardinality(ARRAY[1],1)", "42883"),
            ("SELECT array_remove(ARRAY[1],1,2)", "42883"),
            ("SELECT array_replace(ARRAY[1],1)", "42883"),
            ("SELECT cardinality(1)", "42883"),
            ("SELECT cardinality(NULL)", "42804"),
        ] {
            let crate::Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
                panic!("query")
            };
            let scalar =
                crate::plan::ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
            assert_eq!(
                crate::scalar_type(&scalar, &crate::RowSchema::default(), &[])
                    .unwrap_err()
                    .sqlstate(),
                Some(state),
                "{sql}"
            );
        }
    }
}
