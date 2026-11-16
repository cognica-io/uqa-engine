//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::JsonValue;
use crate::{
    json::{
        JsonDiagnostic, JsonInputError, JsonPosition, JsonReadError, JsonReadWork, JsonValueKind,
    },
    memory::{Produced, ProductionControl, ProductionString},
};

mod numbers;
#[cfg(test)]
mod tests;

impl JsonValue<'_> {
    /// Decode a unit enum after generic-value construction. Object cardinality precedes variant selection, and a known object variant requires a unit payload.
    pub async fn read_unit_variant_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        variants: &'static [&'static str],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<usize, JsonInputError> {
        work.checkpoint().await?;
        control.check().map_err(JsonReadError::from)?;
        let (name, payload) = if let Some(name) = self.as_str() {
            (name, None)
        } else if let Some(mut fields) = self.fields() {
            let first = fields.next();
            let Some((name, payload)) = first.filter(|_| fields.next().is_none()) else {
                return Err(JsonDiagnostic::message(
                    "invalid value: map, expected map with a single key",
                    JsonPosition::unlocated(),
                )
                .into());
            };
            (name, Some(payload))
        } else {
            return Err(JsonDiagnostic::projected_type_with_work(
                self,
                "string or map",
                control,
                work,
            )
            .await?
            .into());
        };
        let Some(index) = variants.iter().position(|variant| *variant == name) else {
            let mut text = ProductionString::new(*control);
            let mut begin = 0;
            while begin < name.len() {
                work.checkpoint().await?;
                let mut end = (begin + 4096).min(name.len());
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                text.push_str(&name[begin..end])
                    .map_err(JsonReadError::from)?;
                begin = end;
            }
            return Err(JsonDiagnostic::unknown_variant(
                text.finish().map_err(JsonReadError::from)?,
                variants,
                JsonPosition::unlocated(),
            )
            .into());
        };
        if let Some(payload) = payload.filter(|value| value.kind() != JsonValueKind::Null) {
            return Err(
                JsonDiagnostic::projected_type_with_work(payload, "unit", control, work)
                    .await?
                    .into(),
            );
        }
        Ok(index)
    }

    /// Adopt an independently admitted string from a complete generic value; a type failure retains its original unlocated diagnostic.
    pub async fn read_string_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<String>, JsonInputError> {
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        let Some(value) = self.as_str() else {
            return Err(
                JsonDiagnostic::projected_type_with_work(self, "a string", control, work)
                    .await?
                    .into(),
            );
        };
        let mut output = ProductionString::new(*control);
        output.reserve(value.len()).map_err(JsonReadError::from)?;
        let mut begin = 0;
        while begin < value.len() {
            work.checkpoint().await?;
            let mut end = (begin + 4096).min(value.len());
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            output
                .push_str(&value[begin..end])
                .map_err(JsonReadError::from)?;
            begin = end;
        }
        Ok(output.finish().map_err(JsonReadError::from)?)
    }

    /// Observe a typed boolean after generic decoding without a physical source position.
    pub async fn read_bool_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<bool, JsonInputError> {
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        control.check().map_err(JsonReadError::from)?;
        if let Some(value) = self.as_bool() {
            return Ok(value);
        }
        Err(
            JsonDiagnostic::projected_type_with_work(self, "a boolean", control, work)
                .await?
                .into(),
        )
    }
}
