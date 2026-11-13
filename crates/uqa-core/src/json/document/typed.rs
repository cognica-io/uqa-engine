//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::JsonValue;
use crate::{
    json::{JsonDiagnostic, JsonInputError, JsonReadError, JsonReadWork},
    memory::{Produced, ProductionControl, ProductionString},
};

mod numbers;

impl JsonValue<'_> {
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
