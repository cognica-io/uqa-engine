//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column typing for `PostgreSQL` array transformations.

use super::super::{generation_type_name, GenerationType, SQLError};

pub(super) fn require_signature(
    name: &str,
    argument_names: &[Option<String>],
    args: &[GenerationType],
) -> Result<GenerationType, SQLError> {
    let names = argument_names
        .iter()
        .map(|name| name.as_deref())
        .collect::<Vec<_>>();
    let Some(positions) = uqa_sql::expr::array_transform_argument_positions(name, &names)? else {
        return Err(undefined_function(name, argument_names, args));
    };
    let mut declared = vec![None; args.len()];
    for (argument, position) in args.iter().zip(positions) {
        declared[position] = Some(argument);
    }
    if declared.iter().skip(1).any(|argument| {
        argument.is_some_and(|argument| {
            !matches!(
                argument,
                GenerationType::Boolean | GenerationType::Null | GenerationType::UnknownLiteral(_)
            )
        })
    }) {
        return Err(undefined_function(name, argument_names, args));
    }
    match declared.first().copied().flatten() {
        Some(argument @ GenerationType::Array(_)) => Ok(argument.clone()),
        Some(GenerationType::Null | GenerationType::UnknownLiteral(_)) => Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: "could not determine polymorphic type because input has type unknown".into(),
        }),
        Some(_) | None => Err(undefined_function(name, argument_names, args)),
    }
}

fn undefined_function(
    name: &str,
    argument_names: &[Option<String>],
    args: &[GenerationType],
) -> SQLError {
    let signature = args
        .iter()
        .zip(argument_names)
        .map(|(argument, argument_name)| {
            let argument = generation_type_name(argument);
            argument_name
                .as_ref()
                .map_or(argument.clone(), |name| format!("{name} => {argument}"))
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    }
}
