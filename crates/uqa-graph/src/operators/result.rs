//! Shared graph-result identifiers and payload conversion.

use super::{DocId, GraphStoreError, GraphStoreResult, Value};

pub(super) fn graph_id_value(id: u64, context: &str) -> GraphStoreResult<Value> {
    i64::try_from(id).map(Value::Int).map_err(|_| {
        GraphStoreError::CorruptGraph(format!(
            "{context} graph id {id} exceeds the agtype integer range"
        ))
    })
}

pub(super) fn synthetic_doc_id(index: usize, context: &str) -> GraphStoreResult<DocId> {
    let one_based = index
        .checked_add(1)
        .ok_or_else(|| GraphStoreError::CorruptGraph(format!("{context} result index overflow")))?;
    u64::try_from(one_based)
        .map_err(|_| GraphStoreError::CorruptGraph(format!("{context} result count exceeds u64")))
}
