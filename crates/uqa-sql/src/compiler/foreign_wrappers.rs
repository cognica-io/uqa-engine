//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain foreign-wrapper function and option clauses without early catalog checks.

use super::{extract_string, relations::collect_def_elem_options};
use crate::{
    ast::{CreateForeignWrapper, ForeignWrapperFunctionOption},
    SQLError,
};
use pg_query::{protobuf::CreateFdwStmt, NodeEnum};

pub(super) fn compile(statement: &CreateFdwStmt) -> Result<CreateForeignWrapper, SQLError> {
    let mut functions = Vec::new();
    for option in &statement.func_options {
        let Some(NodeEnum::DefElem(option)) = option.node.as_ref() else {
            return Err(SQLError::Internal(
                "malformed foreign-wrapper function option".into(),
            ));
        };
        let name = match option.arg.as_ref().and_then(|arg| arg.node.as_ref()) {
            None => None,
            Some(NodeEnum::List(list)) => Some(
                list.items
                    .iter()
                    .map(extract_string)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            _ => {
                return Err(SQLError::Internal(
                    "malformed foreign-wrapper function name".into(),
                ))
            }
        };
        functions.push(match option.defname.as_str() {
            "handler" => ForeignWrapperFunctionOption::Handler(name),
            "validator" => ForeignWrapperFunctionOption::Validator(name),
            _ => {
                return Err(SQLError::Internal(
                    "unknown foreign-wrapper function option".into(),
                ))
            }
        });
    }
    Ok(CreateForeignWrapper {
        name: statement.fdwname.clone(),
        functions,
        options: collect_def_elem_options(&statement.options)?,
    })
}
