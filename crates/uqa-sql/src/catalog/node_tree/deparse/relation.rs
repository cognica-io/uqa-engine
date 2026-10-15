//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate the relation context accepted by `pg_get_expr` before opening that relation.

use super::{Field, SQLError};

pub fn validate_relation(value: &Field, has_relation: bool) -> Result<(), SQLError> {
    let mut head = value;
    while let Field::List(items) = head {
        let Some(first) = items.first() else {
            return Ok(());
        };
        head = first;
    }
    if matches!(head, Field::Node(node) if node.kind == "QUERY") {
        return Err(parameter_error("input is a query, not an expression"));
    }
    validate_variables(value, has_relation)
}

fn validate_variables(value: &Field, has_relation: bool) -> Result<(), SQLError> {
    match value {
        Field::Node(node) => {
            if node.kind == "VAR" && node.integer("varlevelsup")? == 0 {
                if !has_relation {
                    return Err(parameter_error("expression contains variables"));
                }
                if node.integer("varno")? != 1 {
                    return Err(parameter_error(
                        "expression contains variables of more than one relation",
                    ));
                }
            }
            for (_, field) in &node.fields {
                validate_variables(field, has_relation)?;
            }
        }
        Field::List(items) => {
            for item in items {
                validate_variables(item, has_relation)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn parameter_error(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: message.into(),
    }
}
