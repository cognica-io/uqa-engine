//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared controlled entry points and nullable ACL paths.

use super::{reader, Input, JsonToken, Result};
use crate::{
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

pub(super) fn decode<'a, 'c, T>(
    bytes: &'a [u8],
    memory: &'c MemoryBudget,
    cancellation: &'c CancellationToken,
    operation: impl for<'r> FnOnce(&mut Input<'a, 'c, 'r>, JsonToken<'a>) -> Result<T>,
) -> Result<T> {
    let mut tokens = reader(bytes, memory, cancellation);
    let mut input = Input::new(&mut tokens, memory, cancellation);
    let first = input.next()?;
    let value = operation(&mut input, first)?;
    input.end()?;
    Ok(value)
}

pub(super) fn optional<'a, 'c, 'r, T>(
    input: &mut Input<'a, 'c, 'r>,
    first: JsonToken<'a>,
    entry: impl FnMut(&mut Input<'a, 'c, 'r>, JsonToken<'a>) -> Result<Budgeted<T>>,
) -> Result<Budgeted<Option<Vec<T>>>> {
    if first == JsonToken::Null {
        Ok(Budgeted::new(None, input.memory.empty_reservation()))
    } else {
        input
            .acl_with(first, entry)?
            .try_map(|values| Ok(Some(values)))
    }
}
