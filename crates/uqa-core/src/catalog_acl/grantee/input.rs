//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed ACL recipients borrow the complete generic value before adopting their own names.

use super::AclGrantee;
use crate::{
    json::{
        JsonDiagnostic, JsonInputError, JsonPosition, JsonReadError, JsonReadWork, JsonValue,
        JsonValueKind,
    },
    memory::{Produced, ProductionControl},
};

impl AclGrantee {
    /// Read the current tagged or legacy recipient from an already decoded generic value. PUBLIC and a quoted role named PUBLIC retain their distinct meaning; only completed owned names escape the input generation.
    pub async fn from_json_value_with_work<W: JsonReadWork<Error = JsonReadError>>(
        value: JsonValue<'_>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<Self>, JsonInputError> {
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        let name = if let Some(name) = value.as_str() {
            (name != "PUBLIC").then_some(value)
        } else {
            tagged(value, control, work).await?
        };
        let Some(name) = name else {
            return Ok(control
                .finish(Self::Public, control.empty_reservation())
                .map_err(JsonReadError::from)?);
        };
        let (name, memory) = name
            .read_string_with_work(control, work)
            .await?
            .into_parts();
        Ok(control
            .finish(Self::Role(name), memory)
            .map_err(JsonReadError::from)?)
    }
}

async fn tagged<'a, W: JsonReadWork<Error = JsonReadError>>(
    value: JsonValue<'a>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<JsonValue<'a>>, JsonInputError> {
    let (kind, name, numeric_tag) = if let Some(fields) = value.fields() {
        let mut kind = None;
        let mut name = None;
        for (key, value) in fields {
            work.checkpoint().await?;
            control.check().map_err(JsonReadError::from)?;
            match key {
                "kind" => kind = Some(value),
                "name" => name = Some(value),
                _ => return Err(invalid()),
            }
        }
        (kind.ok_or_else(invalid)?, name, false)
    } else if let Some(mut items) = value.elements() {
        if items.len() != 2 {
            return Err(invalid());
        }
        let kind = items.next().ok_or_else(invalid)?;
        let name = items.next();
        if items.next().is_some() {
            return Err(invalid());
        }
        (kind, name, true)
    } else {
        return Err(invalid());
    };
    let tag = kind.as_str().or_else(|| {
        if numeric_tag {
            match kind.number_text() {
                Some("0") => Some("public"),
                Some("1") => Some("role"),
                _ => None,
            }
        } else {
            None
        }
    });
    match tag {
        Some("public") => {
            if name.is_some_and(|value| value.kind() != JsonValueKind::Null) {
                return Err(invalid());
            }
            Ok(None)
        }
        Some("role") => name
            .filter(|value| value.as_str().is_some())
            .map(Some)
            .ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

fn invalid() -> JsonInputError {
    JsonDiagnostic::message(
        "data did not match any variant of untagged enum Stored",
        JsonPosition::unlocated(),
    )
    .into()
}

#[cfg(test)]
mod tests;
