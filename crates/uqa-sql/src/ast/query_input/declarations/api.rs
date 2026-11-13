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
            /// Reconstruct an already decoded generic declaration without reparsing its physical JSON. Work, allocation and both cancellation scopes belong to the caller; typed diagnostics retain their original unlocated messages.
            pub async fn from_projected_json_with_work<
                W: uqa_core::json::JsonReadWork<Error = uqa_core::json::JsonReadError>,
            >(
                value: uqa_core::json::JsonValue<'_>,
                control: &uqa_core::memory::ProductionControl<'_>,
                work: &mut W,
                protocol: uqa_core::json::JsonScalarProtocol,
            ) -> Result<Produced<Self>, uqa_core::json::JsonInputError> {
                let input = crate::ast::query_input::projected::Input {
                    value,
                    control: *control,
                    protocol,
                    buffered: false,
                };
                input.run::<Self, _>(work)?.await.map(Into::into)
            }

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

impl CreateFunction {
    /// Consume prepared native authority in this declaration's original field order. Its allocations must belong to the supplied control. Nested declarations retain ordinary stored authority semantics.
    pub async fn from_projected_json_with_authority<
        W: uqa_core::json::JsonReadWork<Error = uqa_core::json::JsonReadError>,
    >(
        value: uqa_core::json::JsonValue<'_>,
        authority: Produced<crate::routines::security::binding::BoundRoutineAuthority>,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut W,
        protocol: uqa_core::json::JsonScalarProtocol,
    ) -> Result<Produced<Self>, uqa_core::json::JsonInputError> {
        use crate::ast::query_input::projected;
        let input = projected::Input {
            value,
            control: *control,
            protocol,
            buffered: false,
        };
        projected::run_with(
            input,
            Some(authority),
            work,
            super::definitions::projected_function,
        )?
        .await
        .map(Into::into)
    }
}
