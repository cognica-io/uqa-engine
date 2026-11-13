//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, Native, Stored, Values};
use crate::{
    ast::{CreateFunction, Statement},
    SQLError,
};
use uqa_core::{
    memory::{MemoryBudget, Produced},
    CancellationToken,
};

fn read<T: Stored>(text: &str, values: &Values<'_>) -> Result<Produced<T>, InputError> {
    let mut input = Input::new(text, values.control());
    let token = input.next()?;
    let result: Native<T> = input.stored(token, values)?;
    input.end()?;
    Ok(result.into())
}

macro_rules! entry {
    ($ty:ty) => {
        impl $ty {
            /// Construct native stored syntax under the original allocation and cancellation owners, without executing or semantically binding it.
            pub fn from_stored_json(
                text: &str,
                memory: &MemoryBudget,
                original: &CancellationToken,
                invoking: &CancellationToken,
            ) -> Result<Produced<Self>, SQLError> {
                Ok(read(
                    text,
                    &Values {
                        memory,
                        original,
                        invoking,
                    },
                )?)
            }
        }
    };
}
entry!(Statement);
entry!(CreateFunction);
