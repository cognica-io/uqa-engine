//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact evaluated model images. ML owns model shape validation; Storage owns controlled I/O.

use crate::{DeepModel, GatingSpec};
use std::io::{Read, Write};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedVec, MemoryReservation},
};
use uqa_storage::{
    image_io::{ImageOutput, ImagePreparation, ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

mod capture;
mod layers;
mod stream;
pub use capture::ModelImageSnapshot;
pub(crate) use capture::{retained_model_bytes, ModelRetention};
#[cfg(test)]
mod tests;

const MAGIC: &[u8; 8] = b"UQAMOD01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid evaluated model image".into())
}

async fn validate(model: &DeepModel, input: &mut AsyncImageInput<'_>) -> StorageBackendResult<()> {
    let mut validation = crate::deep_fusion::ModelValidation::model(&model.layers, model.alpha)
        .map_err(validation_error)?;
    loop {
        input.checkpoint().await?;
        if validation.advance().map_err(validation_error)? {
            break;
        }
    }
    input.checkpoint().await.map_err(Into::into)
}

fn validation_error(error: crate::MLError) -> StorageBackendError {
    StorageBackendError::backend("model image validation", error)
}

/// Borrow one evaluated model with bounded pre-output validation and exact continuation positions.
pub fn model_image_reader<'a>(
    model: &'a DeepModel,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    ImageReader::new(
        stream::ModelImageCursor::new(model, control, None)?,
        control,
    )
}

/// Stream a valid stored model's exact layer order, scalar bits and owned labels without retraining or constructing another set of weights. Synchronous output drains the same bounded codec as suspended reads.
pub fn write_model_image(
    model: &DeepModel,
    writer: &mut dyn Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    write_parts(model, writer, control, None)
}

fn write_parts(
    model: &DeepModel,
    writer: &mut dyn Write,
    control: &StorageReadControl,
    original: Option<&StorageReadControl>,
) -> StorageBackendResult<u64> {
    let mut cursor = stream::ModelImageCursor::new(model, control, original)?;
    while !cursor.prepare(128)?.ready {}
    let mut output = ImageOutput {
        writer,
        control,
        written: 0,
    };
    let mut buffer = [0; 4096];
    loop {
        let count = cursor.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.bytes(&buffer[..count])?;
    }
    control.check()?;
    Ok(output.written)
}

/// Decode one private model under the original input allowance. Complete framing and the same borrowed validator used by ordinary model/runtime construction precede exposure. Model validation polls cancellation at fixed metadata and numeric windows; framed payload I/O also polls in bounded windows.
pub fn read_model_image(
    reader: &mut dyn Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<DeepModel>> {
    uqa_core::image_io::decode_image(
        reader,
        control.memory(),
        control.cancellation(),
        async |mut input| read_model_image_from(&mut input).await,
    )
}

/// Decode and validate through the enclosing native input channel. Scalar bits, UTF-8 fields and fixed numeric windows resume without replay; the private model retains its original payload admission after completion.
pub async fn read_model_image_from(
    io: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<DeepModel>> {
    if &io.array::<8>().await? != MAGIC {
        return Err(invalid());
    }
    let memory = io.memory.reserve(size_of::<DeepModel>())?;
    let mut input = ModelInput { io, memory };
    let alpha = f64::from_bits(u64::from_le_bytes(input.io.array().await?));
    let gating = match input.io.array::<1>().await?[0] {
        0 => GatingSpec::None,
        1 => GatingSpec::ReLU,
        2 => GatingSpec::Swish,
        _ => return Err(invalid()),
    };
    let count = input.io.count().await?;
    let mut layers = BudgetedVec::new(input.io.memory);
    layers.reserve(count)?;
    for _ in 0..count {
        layers.push(layers::read(&mut input).await?)?;
    }
    input.io.end().await?;
    let (layers, memory) = layers.into_parts();
    input.memory.absorb(memory);
    let model = Budgeted::new(
        DeepModel {
            layers,
            alpha,
            gating,
        },
        input.memory,
    );
    validate(&model, input.io).await?;
    Ok(model)
}

struct ModelInput<'a, 'b> {
    io: &'a mut AsyncImageInput<'b>,
    memory: MemoryReservation,
}

impl ModelInput<'_, '_> {
    async fn floats(&mut self) -> StorageBackendResult<Vec<f64>> {
        let count = self.io.count().await?;
        let mut values = BudgetedVec::new(self.io.memory);
        values.reserve(count)?;
        let mut buffer = [0_u8; 4096];
        while values.len() < count {
            let length = (count - values.len()).min(buffer.len() / 8) * 8;
            self.io.exact(&mut buffer[..length]).await?;
            for bytes in buffer[..length].chunks_exact(8) {
                values.push(f64::from_bits(u64::from_le_bytes(
                    bytes.try_into().expect("eight bytes"),
                )))?;
            }
        }
        let (values, memory) = values.into_parts();
        self.memory.absorb(memory);
        Ok(values)
    }

    async fn text(&mut self) -> StorageBackendResult<String> {
        let (text, memory) = self.io.text().await?.into_parts();
        self.memory.absorb(memory);
        Ok(text)
    }

    async fn float(&mut self) -> StorageBackendResult<f64> {
        Ok(f64::from_bits(u64::from_le_bytes(self.io.array().await?)))
    }

    async fn boolean(&mut self) -> StorageBackendResult<bool> {
        match self.io.array::<1>().await?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid()),
        }
    }
}
