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
    integer!(read_i16_with_work, i16, 6);
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

macro_rules! buffered_integer {
    ($name:ident, $ty:ty) => {
        #[doc = concat!("Read a `", stringify!($ty), "` after generic content buffering, preserving native numeric visitor range errors.")]
        pub async fn $name<W: JsonReadWork<Error = JsonReadError>>(
            self, control: &ProductionControl<'_>, work: &mut W,
        ) -> Result<$ty, JsonInputError> {
            self.buffered_integer_with_work(stringify!($ty), control, work).await
        }
    };
}

impl JsonValue<'_> {
    buffered_integer!(read_buffered_u8_with_work, u8);
    buffered_integer!(read_buffered_u32_with_work, u32);
    buffered_integer!(read_buffered_u64_with_work, u64);
    buffered_integer!(read_buffered_usize_with_work, usize);
    buffered_integer!(read_buffered_i16_with_work, i16);
    buffered_integer!(read_buffered_i32_with_work, i32);
    buffered_integer!(read_buffered_i64_with_work, i64);

    async fn buffered_integer_with_work<
        T: TryFrom<u64> + TryFrom<i64>,
        W: JsonReadWork<Error = JsonReadError>,
    >(
        self,
        expected: &'static str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<T, JsonInputError> {
        use super::super::project::numbers::{observe, Observation};
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        control.check().map_err(JsonReadError::from)?;
        if let Some(text) = self.number_text() {
            match observe(text) {
                Observation::Unsigned(value) => {
                    return T::try_from(value).map_err(|_| {
                        JsonDiagnostic::invalid_unsigned(value, expected, JsonPosition::unlocated())
                            .into()
                    })
                }
                Observation::Signed(value) => {
                    return T::try_from(value).map_err(|_| {
                        JsonDiagnostic::invalid_signed(value, expected, JsonPosition::unlocated())
                            .into()
                    })
                }
                _ => (),
            }
        }
        Err(
            JsonDiagnostic::buffered_type_with_work(self, expected, control, work)
                .await?
                .into(),
        )
    }

    /// Read the float visitor retained by generic content buffering, including native-width integer conversion.
    pub async fn read_buffered_f32_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<f32, JsonInputError> {
        use super::super::project::numbers::{observe, Observation};
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        control.check().map_err(JsonReadError::from)?;
        if let Some(text) = self.number_text() {
            match observe(text) {
                Observation::Unsigned(value) => return Ok(value as f32),
                Observation::Signed(value) => return Ok(value as f32),
                Observation::Float(value) => return Ok(value as f32),
                _ => (),
            }
        }
        Err(
            JsonDiagnostic::buffered_type_with_work(self, "f32", control, work)
                .await?
                .into(),
        )
    }
}

impl JsonValue<'_> {
    /// Read a nonzero identity at its direct generic-value boundary.
    pub async fn read_nonzero_u32_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<std::num::NonZeroU32, JsonInputError> {
        let value = self
            .integer_with_work::<u32, _>(10, "a nonzero u32", control, work)
            .await?;
        std::num::NonZeroU32::new(value).ok_or_else(|| {
            JsonDiagnostic::invalid_unsigned(0, "a nonzero u32", JsonPosition::unlocated()).into()
        })
    }

    /// Read the same nonzero identity after generic content buffering.
    pub async fn read_buffered_nonzero_u32_with_work<W: JsonReadWork<Error = JsonReadError>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<std::num::NonZeroU32, JsonInputError> {
        let value = self
            .buffered_integer_with_work::<u32, _>("a nonzero u32", control, work)
            .await?;
        std::num::NonZeroU32::new(value).ok_or_else(|| {
            JsonDiagnostic::invalid_unsigned(0, "a nonzero u32", JsonPosition::unlocated()).into()
        })
    }
}

#[cfg(test)]
mod tests;
