//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ExecBuildSlotPartitionKeyDescription`: the failing row's partition key for routing errors. The description is omitted unless the current role may read the whole table or every key column, and expression keys require table access.

use super::key::KeyColumn;
use super::PartitionContext;
use crate::expr::EngineHook;
use crate::result::format_postgres_text;
use crate::semantics::row_description::clip_field;
use crate::SQLError;
use uqa_core::Value;

pub(super) fn partition_key_detail(
    context: &PartitionContext<'_>,
    table: &str,
    keys: &[KeyColumn],
    values: &[Value],
) -> Result<Option<String>, SQLError> {
    let columns = keys
        .iter()
        .map(|key| (!key.expression).then_some(key.name.as_str()))
        .collect::<Vec<_>>();
    if !context.catalog.can_view_partition_key(table, &columns)? {
        return Ok(None);
    }
    let engine: &dyn EngineHook = context.assignment;
    let names = keys
        .iter()
        .map(|key| key.description.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut rendered = Vec::with_capacity(values.len());
    for (value, key) in values.iter().zip(keys) {
        rendered.push(if matches!(value, Value::Null) {
            "null".to_string()
        } else {
            clip_field(format_postgres_text(value, &key.ty, Some(engine))?)
        });
    }
    Ok(Some(format!(
        "Partition key of the failing row contains ({names}) = ({}).",
        rendered.join(", ")
    )))
}
