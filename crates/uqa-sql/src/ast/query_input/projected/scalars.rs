//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    finish, frame, native, Input, JsonDiagnostic, JsonPosition, JsonReadError, JsonValueKind,
    Parts, Projected, Read, Result,
};
use uqa_core::memory::ProductionVec;

impl Projected for uqa_core::Value {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            native(
                input
                    .value
                    .read_native_value_with_work(&input.control, work, input.protocol)
                    .await?,
                input.control,
            )
        })
    }
}

impl Projected for String {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            if input.value.as_str().is_none() {
                return Err(input.type_error("a string", work).await?);
            }
            native(
                input
                    .value
                    .read_string_with_work(&input.control, work)
                    .await?,
                input.control,
            )
        })
    }
}
impl Projected for bool {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            input.checkpoint(work).await?;
            match input.value.as_bool() {
                Some(value) => input.scalar(value),
                None => Err(input.type_error("a boolean", work).await?),
            }
        })
    }
}
macro_rules! numbers {
    ($($ty:ty => $read:ident, $buffered:ident);* $(,)?) => {$(
        impl Projected for $ty {
            fn read_value<'i, 'a>(input: Input<'i>, work: &'a mut crate::ast::query_input::projected::Work<'i>) -> Result<Read<'a, Self>> {
                frame(input.control, async move {
                    let value = if input.buffered { input.value.$buffered(&input.control, work).await? } else { input.value.$read(&input.control, work).await? };
                    input.scalar(value)
                })
            }
        }
    )*};
}
numbers!(
    u8 => read_u8_with_work, read_buffered_u8_with_work;
    u32 => read_u32_with_work, read_buffered_u32_with_work;
    std::num::NonZeroU32 => read_nonzero_u32_with_work, read_buffered_nonzero_u32_with_work;
    u64 => read_u64_with_work, read_buffered_u64_with_work;
    usize => read_usize_with_work, read_buffered_usize_with_work;
    i16 => read_i16_with_work, read_buffered_i16_with_work;
    i32 => read_i32_with_work, read_buffered_i32_with_work;
    i64 => read_i64_with_work, read_buffered_i64_with_work
);

impl<T: Projected> Projected for Option<T> {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            input.checkpoint(work).await?;
            if input.value.kind() == JsonValueKind::Null {
                return input.scalar(None);
            }
            let value = input.read::<T>(work).await?;
            let mut parts = Parts::new(input.control);
            let value = Some(parts.take(value));
            finish(parts, value)
        })
    }
}
impl<T: Projected> Projected for Vec<T> {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            input.checkpoint(work).await?;
            let Some(values) = input.value.elements() else {
                return Err(input.type_error("a sequence", work).await?);
            };
            let values = super::iterator(input.control, values)?;
            let mut output = ProductionVec::new(input.control);
            for value in values {
                input.checkpoint(work).await?;
                output
                    .push_produced(input.child(value).read::<T>(work).await?.into())
                    .map_err(JsonReadError::from)?;
            }
            native(output.finish().map_err(JsonReadError::from)?, input.control)
        })
    }
}
impl<T: Projected> Projected for Box<T> {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let value = input.read::<T>(work).await?;
            let header = input
                .control
                .reserve(size_of::<T>())
                .map_err(JsonReadError::from)?;
            let (value, memory) = value.into_parts();
            native(
                input
                    .control
                    .finish(Box::new(value), input.control.combine(memory, header))
                    .map_err(JsonReadError::from)?,
                input.control,
            )
        })
    }
}
impl<A: Projected, B: Projected> Projected for (A, B) {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            input.checkpoint(work).await?;
            let Some(mut values) = input.value.elements() else {
                return Err(input.type_error("a tuple of size 2", work).await?);
            };
            let length = values.len();
            let first = values.next().ok_or_else(|| {
                JsonDiagnostic::invalid_length(0, "a tuple of size 2", JsonPosition::unlocated())
            })?;
            let first = input.child(first).read::<A>(work).await?;
            let second = values.next().ok_or_else(|| {
                JsonDiagnostic::invalid_length(1, "a tuple of size 2", JsonPosition::unlocated())
            })?;
            let second = input.child(second).read::<B>(work).await?;
            if values.next().is_some() {
                return Err(JsonDiagnostic::invalid_length(
                    length,
                    "fewer elements in array",
                    JsonPosition::unlocated(),
                )
                .into());
            }
            let mut parts = Parts::new(input.control);
            let output = (parts.take(first), parts.take(second));
            finish(parts, output)
        })
    }
}
