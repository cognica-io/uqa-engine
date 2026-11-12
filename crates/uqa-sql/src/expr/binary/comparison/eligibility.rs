//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary probes and native image validation share exact fallible-key classification.

use crate::SQLError;
use uqa_core::{image_io::AsyncImageInput, memory::BudgetedVec, Value};

/// Whether an opaque value key can suppress a SQL operator failure. A single such input remains legal until an operator compares it.
pub fn value_comparison_can_fail(value: &Value) -> bool {
    let (fallible, children) = children(value);
    fallible || children.is_some_and(|mut children| children.any(value_comparison_can_fail))
}

/// Observe the same policy through an admitted traversal, yielding between native nodes and parent transitions without reading bytes or interpreting opaque values.
pub async fn value_comparison_can_fail_from(
    value: &Value,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool, SQLError> {
    let mut stack = BudgetedVec::new(input.memory);
    let mut next = Some(value);
    loop {
        input.checkpoint().await?;
        if let Some(value) = next.take() {
            let (fallible, children) = children(value);
            if fallible {
                return Ok(true);
            }
            if let Some(children) = children {
                stack.push(children)?;
            }
        } else if let Some(children) = stack.last_mut() {
            next = children.next();
            if next.is_none() {
                stack.pop();
            }
        } else {
            return Ok(false);
        }
    }
}

fn children(value: &Value) -> (bool, Option<std::slice::Iter<'_, Value>>) {
    match value {
        Value::Datum(_) => (true, None),
        Value::LegacyVector(vector) => (
            vector.kind() == uqa_core::LegacyVectorKind::Oid && !vector.has_vector_layout(),
            None,
        ),
        Value::Array(array) => (
            array.element_type_oid().is_some(),
            Some(array.elements().iter()),
        ),
        Value::Row(values) => (values.field_types().is_some(), Some(values.iter())),
        Value::List(values) => (false, Some(values.iter())),
        Value::Record(fields) => (fields.type_oid().is_some(), Some(fields.values().iter())),
        _ => (false, None),
    }
}

#[cfg(test)]
mod tests;
