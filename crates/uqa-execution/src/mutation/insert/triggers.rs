//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::mutation::triggers::queue::StatementEvent;

/// The relations and operations an INSERT into `table` writes, in the order their AFTER STATEMENT triggers fire: the UPDATE of an `ON CONFLICT DO UPDATE`, then the INSERT unless the statement's rules replaced it.
pub fn insert_statement_events(
    table: &str,
    insert_original_query: bool,
    conflict_update_columns: Option<&[String]>,
) -> Vec<StatementEvent> {
    conflict_update_columns
        .map(|columns| StatementEvent::new(table, uqa_sql::ast::TriggerEvent::Update, columns))
        .into_iter()
        .chain(
            insert_original_query
                .then(|| StatementEvent::new(table, uqa_sql::ast::TriggerEvent::Insert, &[])),
        )
        .collect()
}
