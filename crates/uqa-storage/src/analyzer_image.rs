//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Framing for one resolved descriptor and its exact immutable dictionary closure.

use crate::{
    image_io::{ImageOutput, ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::{
    io::{Read, Write},
    sync::Arc,
};
use uqa_analysis::{
    AnalyzerDescriptor, AnalyzerResource, AnalyzerResourceInput, AnalyzerResourceKind,
    AnalyzerResources, CompiledAnalyzer,
};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput},
    memory::BudgetedVec,
};

mod capture;
mod stream;
pub use capture::AnalyzerImageSnapshot;

const MAGIC: &[u8; 8] = b"UQAANA01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid analyzer image".into())
}

/// Stream resolved configuration and borrowed dictionary artifacts without reopening paths or consulting named resources. The enclosing Engine owner supplies the jointly captured analyzer revision.
pub fn write_analyzer_image(
    analyzer: &CompiledAnalyzer,
    writer: &mut dyn Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    write_parts(
        analyzer.descriptor().canonical_json(),
        analyzer.dictionary_resources().count(),
        analyzer.dictionary_resources(),
        writer,
        control,
    )
}

fn write_parts<'a>(
    descriptor: &'a str,
    count: usize,
    resources: impl Iterator<Item = AnalyzerResource<'a>> + Send,
    writer: &mut dyn Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut cursor = stream::AnalyzerImageCursor::new(descriptor, count, resources, control)?;
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

fn reader_parts<'a>(
    descriptor: &'a str,
    count: usize,
    resources: impl Iterator<Item = AnalyzerResource<'a>> + Send + 'a,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    ImageReader::new(
        stream::AnalyzerImageCursor::new(descriptor, count, resources, control)?,
        control,
    )
}

/// Reconstruct one complete private revision from its supplied artifacts. Encoded dictionaries retain their original input allowance through the final compiled owner. Descriptor preparation and decoded models use the destination Analysis owner's independent validation/resource limits.
pub fn read_analyzer_image(
    reader: &mut dyn Read,
    resources: &AnalyzerResources,
    control: &StorageReadControl,
) -> StorageBackendResult<Arc<CompiledAnalyzer>> {
    decode_image(
        reader,
        control.memory(),
        control.cancellation(),
        async |mut input| read_analyzer_image_from(&mut input, resources).await,
    )
}

/// Read one complete private analyzer through its enclosing image channel. Framing, descriptor preparation, exact dictionary input and native reconstruction retain the original byte/poll window and input reservations.
pub async fn read_analyzer_image_from(
    input: &mut AsyncImageInput<'_>,
    resources: &AnalyzerResources,
) -> StorageBackendResult<Arc<CompiledAnalyzer>> {
    let control = StorageReadControl::new(input.memory, input.cancellation);
    if &input.array::<8>().await? != MAGIC {
        return Err(invalid());
    }
    let encoded = input
        .bytes_with_limit(resources.limits().max_descriptor_bytes)
        .await?;
    let descriptor = AnalyzerDescriptor::from_json_with_input(
        std::str::from_utf8(&encoded).map_err(|_| invalid())?,
        resources.limits(),
        input,
    )
    .await?;
    drop(encoded);
    let count = input.count().await?;
    control.check_value_size(count, resources.limits().max_stages.saturating_add(2))?;
    let mut artifacts = BudgetedVec::new(control.memory());
    artifacts.reserve(count)?;
    let mut previous = None;
    for _ in 0..count {
        let kind = match input.array::<1>().await?[0] {
            0 => AnalyzerResourceKind::Nori,
            1 => AnalyzerResourceKind::Kuromoji,
            _ => return Err(invalid()),
        };
        let limit = resources.image_resource_byte_limit(kind)?;
        let sha256 = input.array().await?;
        if previous.is_some_and(|previous| previous >= (kind, sha256)) {
            return Err(invalid());
        }
        let bytes = input.bytes_with_limit(limit).await?.into_shared()?;
        artifacts.push(AnalyzerResourceInput {
            kind,
            sha256,
            bytes,
        })?;
        previous = Some((kind, sha256));
    }
    input.end().await?;
    let (artifacts, _memory) = artifacts.into_parts();
    control.check()?;
    let analyzer = resources
        .restore_image_with_input(descriptor, artifacts, input)
        .await?;
    control.check()?;
    Ok(analyzer)
}

#[cfg(test)]
mod tests;
