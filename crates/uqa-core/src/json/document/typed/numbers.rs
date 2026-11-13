//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer projection preserves generic-value widths and unlocated diagnostics.

use super::super::JsonValue;
use crate::{
    json::{
        JsonDiagnostic, JsonInputError, JsonPosition, JsonReadError, JsonReadWork, JsonSyntaxError,
        JsonSyntaxKind,
    },
    memory::ProductionControl,
};

macro_rules! integer {
    ($name:ident, $ty:ty, $length:expr) => {
        #[doc = concat!("Read a typed `", stringify!($ty), "` from a complete generic value without allocating scalar storage or a physical source position.")]
        pub async fn $name<W: JsonReadWork<Error = JsonReadError>>(
            self,
            control: &ProductionControl<'_>,
            work: &mut W,
        ) -> Result<$ty, JsonInputError> {
            self.integer_with_work($length, stringify!($ty), control, work).await
        }
    };
}

impl JsonValue<'_> {
    integer!(read_u8_with_work, u8, 3);
    integer!(read_u32_with_work, u32, 10);
    integer!(read_u64_with_work, u64, 20);
    integer!(read_usize_with_work, usize, 20);
    integer!(read_i32_with_work, i32, 11);
    integer!(read_i64_with_work, i64, 20);

    async fn integer_with_work<T: std::str::FromStr, W: JsonReadWork<Error = JsonReadError>>(
        self,
        maximum_length: usize,
        expected: &'static str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<T, JsonInputError> {
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        control.check().map_err(JsonReadError::from)?;
        if let Some(text) = self.number_text() {
            if text.len() <= maximum_length {
                if let Ok(value) = text.parse() {
                    return Ok(value);
                }
            }
            return Err(JsonDiagnostic::from(JsonSyntaxError {
                kind: JsonSyntaxKind::InvalidNumber,
                position: JsonPosition::unlocated(),
            })
            .into());
        }
        Err(
            JsonDiagnostic::projected_type_with_work(self, expected, control, work)
                .await?
                .into(),
        )
    }
}

#[cfg(test)]
mod tests;
