//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Untagged declarations buffer their original generic observations before selecting an alternative.

use super::{
    finish, Input, JsonDiagnostic, JsonInputError, JsonPosition, JsonReadError, JsonReadWork,
    JsonValueKind, Native, Parts, Result,
};
use crate::ast::{AutoIncrement, SequenceRestart};
use uqa_core::catalog_role::RoleIdentity;

pub(super) fn untagged<T>(value: Result<T>, name: &'static str) -> Result<T> {
    match value {
        Err(JsonInputError::Diagnostic(_)) => Err(JsonDiagnostic::prefixed_message(
            "data did not match any variant of untagged enum ",
            name,
            JsonPosition::unlocated(),
        )
        .into()),
        result => result,
    }
}

pub(in crate::ast::query_input) async fn read_owner(
    input: Input<'_>,
    work: &mut impl JsonReadWork<Error = JsonReadError>,
) -> Result<Native<Option<RoleIdentity>>> {
    let prepared = input.buffer(work).await?;
    let input = input.buffered_value(prepared.as_ref());
    if let Some(name) = input.value.as_str() {
        if name.is_empty() {
            return input.scalar(None);
        }
        return Err(JsonDiagnostic::message(
            "routine owner requires a role identity",
            JsonPosition::unlocated(),
        )
        .into());
    }
    let result = input.run::<_, _>(work)?.await;
    untagged(result, "Owner")
}

pub(in crate::ast::query_input) async fn read_estimate(
    input: Input<'_>,
    work: &mut impl JsonReadWork<Error = JsonReadError>,
) -> Result<Native<Option<f32>>> {
    let prepared = input.buffer(work).await?;
    let input = input.buffered_value(prepared.as_ref());
    if input.value.kind() == JsonValueKind::Null {
        return input.scalar(None);
    }
    if let Some(text) = input.value.as_str() {
        let value = match text {
            "Infinity" => f32::INFINITY,
            "-Infinity" => f32::NEG_INFINITY,
            "NaN" => f32::NAN,
            _ => {
                return Err(JsonDiagnostic::retained_message(
                    &const {
                        uqa_core::json::JsonMessageFormat::new(
                            "invalid routine estimate `",
                            "`",
                            "invalid routine estimate",
                        )
                    },
                    input.text(text, work).await?,
                    JsonPosition::unlocated(),
                )
                .into())
            }
        };
        return input.scalar(Some(value));
    }
    let value = untagged(
        input
            .value
            .read_buffered_f32_with_work(&input.control, work)
            .await,
        "Estimate",
    )?;
    input.scalar(Some(value))
}

pub(in crate::ast::query_input) async fn read_auto_increment(
    input: Input<'_>,
    work: &mut impl JsonReadWork<Error = JsonReadError>,
) -> Result<Native<Option<AutoIncrement>>> {
    let prepared = input.buffer(work).await?;
    let input = input.buffered_value(prepared.as_ref());
    if input.value.kind() == JsonValueKind::Null {
        return input.scalar(None);
    }
    if let Some(value) = input.value.as_bool() {
        return input.scalar(value.then(AutoIncrement::legacy));
    }
    let value = untagged(input.run::<AutoIncrement, _>(work)?.await, "Representation")?;
    let mut parts = Parts::new(input.control);
    let result = Some(parts.take(value));
    finish(parts, result)
}

pub(in crate::ast::query_input) async fn read_sequence_restart(
    input: Input<'_>,
    work: &mut impl JsonReadWork<Error = JsonReadError>,
) -> Result<Native<SequenceRestart>> {
    let prepared = input.buffer(work).await?;
    let input = input.buffered_value(prepared.as_ref());
    if input.value.kind() == JsonValueKind::Null {
        return input.scalar(SequenceRestart::Unchanged);
    }
    if input.value.kind() == JsonValueKind::Number {
        let value = untagged(input.run::<i64, _>(work)?.await, "Representation")?
            .into_parts()
            .0;
        return input.scalar(SequenceRestart::With(value));
    }
    let result = input.run::<_, _>(work)?.await;
    untagged(result, "Representation")
}

pub(in crate::ast::query_input) use super::compound::read_explain;
