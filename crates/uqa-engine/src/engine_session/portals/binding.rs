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
        if let Some(canonical) = engine.try_resolve_table_name(&requested).map_err(|error| {
            SQLError::Internal(format!(
                "bind cursor table-function relation `{requested}` at DECLARE: {error}"
            ))
        })? {
            *relation = canonical;
        }
    }
    Ok(())
}
