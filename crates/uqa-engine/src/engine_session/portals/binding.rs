//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cursor declaration-time binding for table-function relations.

use crate::{Engine, SQLError};
use uqa_sql::ast::OperatorJoinRelations;

pub(super) fn bind_session_portal_function_relations(
    engine: &Engine,
    relations: &mut Option<OperatorJoinRelations>,
) -> Result<(), SQLError> {
    let Some(relations) = relations else {
        return Ok(());
    };
    for relation in [&mut relations.left, &mut relations.right] {
        let requested = relation.clone();
        match engine.try_resolve_visible_relation_kind(&requested)? {
            Some((canonical, "table")) => *relation = canonical,
            Some((canonical, kind)) => {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "cursor table-function relation \"{canonical}\" is a {kind}, not a table"
                    ),
                });
            }
            None => return Err(SQLError::UnknownTable(requested)),
        }
    }
    Ok(())
}
