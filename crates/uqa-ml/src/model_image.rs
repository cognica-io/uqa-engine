//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact evaluated model images. ML owns model shape validation; Storage owns controlled I/O.

use crate::{DeepModel, GatingSpec};
use std::io::{Read, Write};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryReservation};
use uqa_storage::{
    image_io::{ImageInput, ImageOutput, ImagePreparation, ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

mod capture;
mod layers;
mod stream;
pub(crate) use capture::retained_model_bytes;
pub use capture::ModelImageSnapshot;
#[cfg(test)]
mod tests;

const MAGIC: &[u8; 8] = b"UQAMOD01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid evaluated model image".into())
}

fn validate(model: &DeepModel, control: &StorageReadControl) -> StorageBackendResult<()> {
    let mut validation = crate::deep_fusion::ModelValidation::model(&model.layers, model.alpha)
        .map_err(validation_error)?;
    loop {
        control.check()?;
        if validation.advance().map_err(validation_error)? {
            break;
        }
    }
    control.check()
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
    let mut io = ImageInput { reader, control };
    if &io.array::<8>()? != MAGIC {
        return Err(invalid());
    }
    let mut input = ModelInput {
        io,
        memory: control.memory().reserve(size_of::<DeepModel>())?,
    };
    let alpha = f64::from_bits(u64::from_le_bytes(input.io.array()?));
    let gating = match input.io.array::<1>()?[0] {
        0 => GatingSpec::None,
        1 => GatingSpec::ReLU,
        2 => GatingSpec::Swish,
        _ => return Err(invalid()),
    };
    let count = input.io.count()?;
    let mut layers = BudgetedVec::new(control.memory());
    layers.reserve(count)?;
    for _ in 0..count {
        layers.push(layers::read(&mut input)?)?;
    }
    input.io.end()?;
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
    control.check()?;
    validate(&model, control)?;
    control.check()?;
    Ok(model)
}

struct ModelInput<'a> {
    io: ImageInput<'a>,
    memory: MemoryReservation,
}

impl ModelInput<'_> {
    fn floats(&mut self) -> StorageBackendResult<Vec<f64>> {
        let count = self.io.count()?;
        let mut values = BudgetedVec::new(self.io.control.memory());
        values.reserve(count)?;
        let mut buffer = [0_u8; 4096];
        while values.len() < count {
            let length = (count - values.len()).min(buffer.len() / 8) * 8;
            self.io.exact(&mut buffer[..length])?;
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

    fn text(&mut self) -> StorageBackendResult<String> {
        let (text, memory) = self.io.text()?.into_parts();
        self.memory.absorb(memory);
        Ok(text)
    }

    fn float(&mut self) -> StorageBackendResult<f64> {
        Ok(f64::from_bits(u64::from_le_bytes(self.io.array()?)))
    }

    fn boolean(&mut self) -> StorageBackendResult<bool> {
        match self.io.array::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid()),
        }
    }
}
