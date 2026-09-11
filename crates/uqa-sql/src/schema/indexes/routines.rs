//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze durable routine references in index keys before the partial-index predicate.

use crate::{
    ast::{FunctionBinding, IndexKey},
    catalog::{
        index::IndexDefinition,
        stored_ast::{expression_references_routine_identity, rewrite_expression_routine_identity},
    },
    SQLError,
};

pub fn index_references_routine(
    keys: &[IndexKey],
    definition: &IndexDefinition,
    target: &FunctionBinding,
) -> Result<bool, SQLError> {
    for expression in keys
        .iter()
        .filter_map(|key| match key {
            IndexKey::Expression(expr) => Some(expr.as_ref()),
            IndexKey::Column(_) => None,
        })
        .chain(definition.predicate.as_deref())
    {
        if expression_references_routine_identity(expression, target)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn rewrite_index_routine_references(
    keys: &mut [IndexKey],
    definition: &mut IndexDefinition,
    target: &FunctionBinding,
    name: &str,
) -> Result<bool, SQLError> {
    let mut changed = false;
    for expression in keys
        .iter_mut()
        .filter_map(|key| match key {
            IndexKey::Expression(expr) => Some(expr.as_mut()),
            IndexKey::Column(_) => None,
        })
        .chain(definition.predicate.as_deref_mut())
    {
        changed |= rewrite_expression_routine_identity(expression, target, name)?;
    }
    Ok(changed)
}
