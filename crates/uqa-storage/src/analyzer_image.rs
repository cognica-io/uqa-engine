//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Framing for one resolved descriptor and its exact immutable dictionary closure.

use crate::{
    image_io::{ImageInput, ImageOutput},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::{
    io::{Read, Write},
    sync::Arc,
};
use uqa_analysis::{
    AnalyzerDescriptor, AnalyzerResourceInput, AnalyzerResourceKind, AnalyzerResources,
    CompiledAnalyzer,
};
use uqa_core::memory::BudgetedVec;

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
    let mut output = ImageOutput {
        writer,
        control,
        written: 0,
    };
    output.bytes(MAGIC)?;
    output.sized(analyzer.descriptor().canonical_json().as_bytes())?;
    output.count(analyzer.dictionary_resources().count())?;
    for resource in analyzer.dictionary_resources() {
        let kind = match resource.kind() {
            AnalyzerResourceKind::Nori => 0,
            AnalyzerResourceKind::Kuromoji => 1,
        };
        output.bytes(&[kind])?;
        output.bytes(&resource.sha256())?;
        output.sized(resource.bytes())?;
    }
    control.check()?;
    Ok(output.written)
}

/// Reconstruct one complete private revision from its supplied artifacts. Encoded dictionaries retain their original input allowance through the final compiled owner. Descriptor preparation and decoded models use the destination Analysis owner's independent validation/resource limits.
pub fn read_analyzer_image(
    reader: &mut dyn Read,
    resources: &AnalyzerResources,
    control: &StorageReadControl,
) -> StorageBackendResult<Arc<CompiledAnalyzer>> {
    let mut input = ImageInput { reader, control };
    if &input.array::<8>()? != MAGIC {
        return Err(invalid());
    }
    let encoded = input.bytes_with_limit(resources.limits().max_descriptor_bytes)?;
    let descriptor = AnalyzerDescriptor::from_json(
        std::str::from_utf8(&encoded).map_err(|_| invalid())?,
        resources.limits(),
    )?;
    drop(encoded);
    let count = input.count()?;
    control.check_value_size(count, resources.limits().max_stages.saturating_add(2))?;
    let mut artifacts = BudgetedVec::new(control.memory());
    artifacts.reserve(count)?;
    let mut previous = None;
    for _ in 0..count {
        let kind = match input.array::<1>()?[0] {
            0 => AnalyzerResourceKind::Nori,
            1 => AnalyzerResourceKind::Kuromoji,
            _ => return Err(invalid()),
        };
        let limit = resources.image_resource_byte_limit(kind)?;
        let sha256 = input.array()?;
        if previous.is_some_and(|previous| previous >= (kind, sha256)) {
            return Err(invalid());
        }
        let bytes = input.bytes_with_limit(limit)?.into_shared()?;
        artifacts.push(AnalyzerResourceInput {
            kind,
            sha256,
            bytes,
        })?;
        previous = Some((kind, sha256));
    }
    input.end()?;
    let (artifacts, _memory) = artifacts.into_parts();
    control.check()?;
    let analyzer = resources.restore_image(descriptor, artifacts)?;
    control.check()?;
    Ok(analyzer)
}

#[cfg(test)]
mod tests;
